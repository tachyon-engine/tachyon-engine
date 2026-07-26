//! Native TC39 Signals graph payloads and the first executable API slice.

use super::super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ComputedState {
    Clean,
    Computing,
    Dirty,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum WatcherState {
    Waiting,
    Watching,
    Pending,
}

#[derive(Debug)]
pub(crate) struct OrderedSignals {
    entries: Vec<Value>,
}

impl OrderedSignals {
    fn try_new() -> Result<Self, ExecutionError> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(tuning::signals::INITIAL_EDGE_CAPACITY)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        Ok(Self { entries })
    }

    fn insert(&mut self, value: Value) -> Result<bool, ExecutionError> {
        if self.entries.contains(&value) {
            return Ok(false);
        }
        if self.entries.len() == self.entries.capacity() {
            self.entries
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        }
        self.entries.push(value);
        Ok(true)
    }

    fn remove(&mut self, value: Value) -> bool {
        let Some(index) = self.entries.iter().position(|entry| *entry == value) else {
            return false;
        };
        self.entries.remove(index);
        true
    }

    fn try_snapshot(&self) -> Result<Vec<Value>, ExecutionError> {
        let mut snapshot = Vec::new();
        snapshot
            .try_reserve_exact(self.entries.len())
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        snapshot.extend_from_slice(&self.entries);
        Ok(snapshot)
    }
}

impl Trace for OrderedSignals {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.entries.trace(tracer);
    }
}

#[derive(Debug)]
#[repr(C)]
pub(crate) struct StateSignal {
    value: Value,
    equals: Value,
    watched: Value,
    unwatched: Value,
    sinks: OrderedSignals,
    live_sinks: u32,
    pub(crate) ordinary: OrdinaryObject,
}

impl Trace for StateSignal {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.value.trace(tracer);
        self.equals.trace(tracer);
        self.watched.trace(tracer);
        self.unwatched.trace(tracer);
        self.sinks.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

#[derive(Debug)]
#[repr(C)]
pub(crate) struct ComputedSignal {
    callback: Value,
    cached: Value,
    state: ComputedState,
    sources: OrderedSignals,
    sinks: OrderedSignals,
    live_sinks: u32,
    generation: u64,
    pub(crate) ordinary: OrdinaryObject,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum SignalWatcherOperationKind {
    Watch,
    Unwatch,
}

/// Traced arguments and hook queue for one resumable Watcher mutation.
#[derive(Debug)]
pub(crate) struct PendingSignalWatcherOperation {
    watcher: Value,
    arguments: Vec<Value>,
    hooks: Vec<Value>,
    argument_index: usize,
    hook_index: usize,
    kind: SignalWatcherOperationKind,
}

#[derive(Debug)]
struct SignalWatcherOperationSnapshot {
    watcher: Value,
    argument: Option<Value>,
    hook: Option<Value>,
    kind: SignalWatcherOperationKind,
}

impl Trace for PendingSignalWatcherOperation {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.watcher.trace(tracer);
        self.arguments.trace(tracer);
        self.hooks.trace(tracer);
    }
}

struct SignalStateRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for SignalStateRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Trace for ComputedSignal {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.callback.trace(tracer);
        self.cached.trace(tracer);
        self.sources.trace(tracer);
        self.sinks.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

#[derive(Debug)]
#[repr(C)]
pub(crate) struct WatcherSignal {
    notify: Value,
    state: WatcherState,
    watched: OrderedSignals,
    pending: OrderedSignals,
    pub(crate) ordinary: OrdinaryObject,
}

impl Trace for WatcherSignal {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.notify.trace(tracer);
        self.watched.trace(tracer);
        self.pending.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

#[derive(Debug, Default)]
pub(crate) struct SignalRuntime {
    pub(crate) computing: Option<Value>,
    pub(crate) generation: u64,
    worklist: Vec<Value>,
}

impl Isolate {
    /// Starts State construction and reads options through ordered observable `Get` operations.
    pub(crate) fn begin_signal_state_constructor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let options = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let prototype = self.signal_prototype_for_new_target(
            site.new_target,
            IntrinsicPrototypeKind::SignalState,
            self.realm
                .signal_state_prototype
                .expect("Signal.State initialized"),
        )?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let state = self
            .heap
            .try_allocate_with_gc(
                self.types.signal_state,
                0,
                0,
                StateSignal {
                    value,
                    equals: Value::from_immediate(Immediate::Undefined),
                    watched: Value::from_immediate(Immediate::Undefined),
                    unwatched: Value::from_immediate(Immediate::Undefined),
                    sinks: OrderedSignals::try_new()?,
                    live_sinks: 0,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                AllocationSpace::Young,
                roots,
            )
            .map(|node| Value::from_heap_ref(node.raw()))
            .map_err(ExecutionError::HeapAllocation)?;
        if is_nullish(options) {
            return self.write(site.caller_base, site.destination, state);
        }
        let pending = self.allocate_signal_state_call_state(NativeCallState {
            values: [
                state,
                options,
                Value::from_immediate(Immediate::Undefined),
                Value::from_immediate(Immediate::Undefined),
                Value::from_immediate(Immediate::Undefined),
            ],
            count: 2,
        })?;
        self.dispatch_signal_state_option_get(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            pending,
            SignalStateStage::OptionsEquals,
        )
    }

    /// Resumes an options getter or the custom equality callback without retaining Rust frames.
    pub(crate) fn resume_signal_state(
        &mut self,
        continuation: NativeContinuation,
        stage: SignalStateStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_reference(continuation.first())?;
        let snapshot = self.native_call_state_snapshot(pending)?;
        let state_value = match stage {
            SignalStateStage::Equals => continuation.second(),
            _ => snapshot.values[0],
        };
        match stage {
            SignalStateStage::OptionsEquals => {
                let equals = if self.is_truthy_value(value)? {
                    value
                } else {
                    Value::from_immediate(Immediate::Undefined)
                };
                self.set_signal_state_option(state_value, SignalStateStage::OptionsEquals, equals)?;
                self.dispatch_signal_state_option_get(
                    continuation.site(),
                    pending,
                    SignalStateStage::OptionsWatched,
                )
            }
            SignalStateStage::OptionsWatched => {
                self.set_signal_state_option(state_value, stage, value)?;
                self.dispatch_signal_state_option_get(
                    continuation.site(),
                    pending,
                    SignalStateStage::OptionsUnwatched,
                )
            }
            SignalStateStage::OptionsUnwatched => {
                self.set_signal_state_option(state_value, stage, value)?;
                self.write(
                    continuation.site().caller_base,
                    continuation.site().destination,
                    state_value,
                )
            }
            SignalStateStage::Equals => {
                if !self.is_truthy_value(value)? {
                    self.commit_signal_state_value(state_value, snapshot.values[1])?;
                }
                self.write(
                    continuation.site().caller_base,
                    continuation.site().destination,
                    Value::from_immediate(Immediate::Undefined),
                )
            }
        }
    }

    /// Reads the next options field with the parent Signal continuation below nested Proxy/Get work.
    fn dispatch_signal_state_option_get(
        &mut self,
        site: NativeContinuationSite,
        pending: GcRef<NativeCallState>,
        stage: SignalStateStage,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(pending)?;
        let options = snapshot.values[1];
        let key = match stage {
            SignalStateStage::OptionsEquals => self.intern_intrinsic_name(b"equals")?.into(),
            SignalStateStage::OptionsWatched => self.property_key(
                self.realm
                    .signal_watched_symbol
                    .expect("Signal.subtle.watched initializes before construction"),
            )?,
            SignalStateStage::OptionsUnwatched => self.property_key(
                self.realm
                    .signal_unwatched_symbol
                    .expect("Signal.subtle.unwatched initializes before construction"),
            )?,
            SignalStateStage::Equals => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
        };
        let continuation = NativeContinuation::signal_state(
            site,
            stage,
            Value::from_heap_ref(pending.raw()),
            snapshot.values[0],
        );
        self.dispatch_signal_nested_operation(continuation, |isolate| {
            isolate
                .dispatch_proxy_aware_property_read(site, options, options, key)
                .map(|_| ())
        })
    }

    /// Drains one synchronous nested property operation or leaves its parent for frame return.
    fn dispatch_signal_nested_operation(
        &mut self,
        continuation: NativeContinuation,
        operation: impl FnOnce(&mut Self) -> Result<(), ExecutionError>,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = operation(self) {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(
            continuation.site().caller_base,
            continuation.site().destination,
        )?;
        let NativeContinuationKind::SignalState(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_signal_state(continuation, stage, value)
    }

    /// Allocates a compact traced State operation record before the first callback can run.
    fn allocate_signal_state_call_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = SignalStateRoots {
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

    /// Stores one option callback and publishes its generational edge.
    fn set_signal_state_option(
        &mut self,
        state: Value,
        stage: SignalStateStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.signal_state_reference(state)?;
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(state, self.types.signal_state)
                    .map_err(ExecutionError::NoGcBorrow)?;
                match stage {
                    SignalStateStage::OptionsEquals => node.equals = value,
                    SignalStateStage::OptionsWatched => node.watched = value,
                    SignalStateStage::OptionsUnwatched => node.unwatched = value,
                    SignalStateStage::Equals => {
                        return Err(ExecutionError::MissingNativeContinuation);
                    }
                }
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Allocates one initially-dirty Computed node after validating its callback.
    pub(crate) fn create_signal_computed_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.resolve_function_object(callback)
            .map_err(|_| ExecutionError::NonCallable(callback))?;
        let prototype = self.signal_prototype_for_new_target(
            site.new_target,
            IntrinsicPrototypeKind::SignalComputed,
            self.realm
                .signal_computed_prototype
                .expect("Signal.Computed initialized"),
        )?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.signal_computed,
                0,
                0,
                ComputedSignal {
                    callback,
                    cached: Value::from_immediate(Immediate::Undefined),
                    state: ComputedState::Dirty,
                    sources: OrderedSignals::try_new()?,
                    sinks: OrderedSignals::try_new()?,
                    live_sinks: 0,
                    generation: 0,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                AllocationSpace::Young,
                roots,
            )
            .map(|node| Value::from_heap_ref(node.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates one Watcher node; notification dispatch is added by the next graph slice.
    pub(crate) fn create_signal_watcher_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let notify = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.resolve_function_object(notify)
            .map_err(|_| ExecutionError::NonCallable(notify))?;
        let prototype = self.signal_prototype_for_new_target(
            site.new_target,
            IntrinsicPrototypeKind::SignalWatcher,
            self.realm
                .signal_watcher_prototype
                .expect("Signal.subtle.Watcher initialized"),
        )?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.signal_watcher,
                0,
                0,
                WatcherSignal {
                    notify,
                    state: WatcherState::Waiting,
                    watched: OrderedSignals::try_new()?,
                    pending: OrderedSignals::try_new()?,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                AllocationSpace::Young,
                roots,
            )
            .map(|node| Value::from_heap_ref(node.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Returns State's value and records the active Computed dependency exactly once.
    pub(crate) fn signal_state_get(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        let state = self.signal_state_reference(receiver)?;
        let value = self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.signal_state)
                    .map(|node| node.value)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        self.record_signal_dependency(receiver)?;
        Ok(value)
    }

    /// Applies default SameValue immediately or dispatches the State's custom comparator.
    pub(crate) fn begin_signal_state_set(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let state = self.signal_state_reference(receiver)?;
        let (old, equals) = self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.signal_state)
                    .map(|node| (node.value, node.equals))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        if equals.as_immediate() == Some(Immediate::Undefined) {
            if !self.same_value(old, value)? {
                self.commit_signal_state_value(receiver, value)?;
            }
            return self.write(
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::Undefined),
            );
        }
        self.resolve_function_object(equals)?;
        let pending = self.allocate_signal_state_call_state(NativeCallState {
            values: [
                old,
                value,
                Value::from_immediate(Immediate::Undefined),
                Value::from_immediate(Immediate::Undefined),
                Value::from_immediate(Immediate::Undefined),
            ],
            count: 2,
        })?;
        self.dispatch_property_callback(
            NativeContinuation::signal_state(
                NativeContinuationSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    call_site: site.call_site,
                },
                SignalStateStage::Equals,
                Value::from_heap_ref(pending.raw()),
                receiver,
            ),
            equals,
        )?;
        Ok(())
    }

    /// Publishes a changed State value, increments generation, and dirties downstream nodes.
    fn commit_signal_state_value(
        &mut self,
        receiver: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.signal_state_reference(receiver)?;
        let sinks = self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(state, self.types.signal_state)
                    .map_err(ExecutionError::NoGcBorrow)?;
                node.value = value;
                node.sinks.try_snapshot()
            })
        })?;
        self.signal_runtime.generation = self.signal_runtime.generation.wrapping_add(1);
        self.propagate_signal_change(sinks)
    }

    /// Returns a cached Computed value or dispatches its callback through a native continuation.
    pub(crate) fn begin_signal_computed_get(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        let computed = self.signal_computed_reference(receiver)?;
        let snapshot = self.computed_snapshot(computed)?;
        if snapshot.0 == ComputedState::Clean {
            self.record_signal_dependency(receiver)?;
            return self.write(site.caller_base, site.destination, snapshot.2);
        }
        if snapshot.0 == ComputedState::Computing {
            return Err(ExecutionError::NotObject(receiver));
        }
        self.detach_computed_sources(receiver, computed, snapshot.1)?;
        let previous = self.signal_runtime.computing.replace(receiver);
        self.set_computed_state(computed, ComputedState::Computing)?;
        self.fiber
            .completions
            .push_native(NativeContinuation::signal_computed(
                NativeContinuationSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    call_site: site.call_site,
                },
                receiver,
                previous.unwrap_or(Value::from_immediate(Immediate::Undefined)),
            ))
            .map_err(|_| ExecutionError::CompletionAllocationFailed)?;
        let frame_depth = self.fiber.frames.len();
        let result = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: snapshot.3,
            argument_base: 0,
            argument_source: None,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 0,
            this_value: receiver,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        });
        if let Err(error) = result {
            self.signal_runtime.computing = previous;
            self.set_computed_state(computed, ComputedState::Dirty)?;
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("callback frame was pushed");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let returned = self.read(site.caller_base, site.destination)?;
        self.resume_signal_computed(continuation, returned)
    }

    /// Commits one successful Computed callback and restores nested dependency tracking.
    pub(crate) fn resume_signal_computed(
        &mut self,
        continuation: NativeContinuation,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let receiver = continuation.first();
        let computed = self.signal_computed_reference(receiver)?;
        self.heap.with_running_scope(|scope| {
            let computed = scope.root(computed).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(computed, self.types.signal_computed)
                    .map_err(ExecutionError::NoGcBorrow)?;
                node.cached = value;
                node.state = ComputedState::Clean;
                node.generation = self.signal_runtime.generation;
                Ok(())
            })
        })?;
        let previous = continuation.second();
        self.signal_runtime.computing =
            (previous.as_immediate() != Some(Immediate::Undefined)).then_some(previous);
        self.record_signal_dependency(receiver)?;
        self.write(
            continuation.site().caller_base,
            continuation.site().destination,
            value,
        )
    }

    /// Adds valid signals to a Watcher's ordered set after complete argument validation.
    pub(crate) fn signal_watcher_watch(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let arguments = self.validated_signal_arguments(site, false)?;
        if site.argument_count == 0 {
            let watcher = self.signal_watcher_reference(site.this_value)?;
            self.heap.with_running_scope(|scope| {
                let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let node = no_gc
                        .borrow_mut(watcher, self.types.signal_watcher)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    node.pending.entries.clear();
                    node.state = WatcherState::Watching;
                    Ok(())
                })
            })?;
            self.write(
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::Undefined),
            )?;
            return Ok(());
        }
        let pending = self.allocate_pending_signal_watcher_operation(
            site.this_value,
            SignalWatcherOperationKind::Watch,
        )?;
        self.set_pending_signal_watcher_arguments(pending, arguments)?;
        self.resume_signal_watcher_operation(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            pending,
        )?;
        Ok(())
    }

    /// Removes validated watched signals from a Watcher without partial mutation on errors.
    pub(crate) fn signal_watcher_unwatch(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let arguments = self.validated_signal_arguments(site, true)?;
        let pending = self.allocate_pending_signal_watcher_operation(
            site.this_value,
            SignalWatcherOperationKind::Unwatch,
        )?;
        self.set_pending_signal_watcher_arguments(pending, arguments)?;
        self.resume_signal_watcher_operation(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            pending,
        )?;
        Ok(())
    }

    /// Allocates a traced operation record before any lifecycle callback can suspend execution.
    fn allocate_pending_signal_watcher_operation(
        &mut self,
        watcher: Value,
        kind: SignalWatcherOperationKind,
    ) -> Result<GcRef<PendingSignalWatcherOperation>, ExecutionError> {
        let mut hooks = Vec::new();
        hooks
            .try_reserve_exact(tuning::signals::INITIAL_OPERATION_CAPACITY)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        let mut roots = VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let pending = self
            .heap
            .try_allocate_with_gc(
                self.types.pending_signal_watcher_operation,
                0,
                0,
                PendingSignalWatcherOperation {
                    watcher: Value::from_immediate(Immediate::Undefined),
                    arguments: Vec::new(),
                    hooks,
                    argument_index: 0,
                    hook_index: 0,
                    kind,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        self.set_pending_signal_watcher_watcher(pending, watcher)?;
        Ok(pending)
    }

    fn set_pending_signal_watcher_watcher(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
        watcher: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(pending, self.types.pending_signal_watcher_operation)
                    .map(|node| node.watcher = watcher)
                    .map_err(ExecutionError::NoGcBorrow)
            })?;
            scope
                .write_value_barrier(pending, watcher)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Publishes validated Watcher arguments after the pending object itself is GC-rooted.
    fn set_pending_signal_watcher_arguments(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
        arguments: Vec<Value>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(pending, self.types.pending_signal_watcher_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                node.arguments
                    .try_reserve_exact(arguments.len())
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                node.arguments.extend(arguments.iter().copied());
                Ok::<(), ExecutionError>(())
            })?;
            for argument in arguments {
                scope
                    .write_value_barrier(pending, argument)
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(())
        })
    }

    /// Resumes one Watcher operation, draining hooks iteratively and preserving argument order.
    fn resume_signal_watcher_operation(
        &mut self,
        site: NativeContinuationSite,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<(), ExecutionError> {
        loop {
            let snapshot = self.pending_signal_watcher_snapshot(pending)?;
            if let Some(signal) = snapshot.hook {
                self.pending_signal_watcher_advance_hook(pending)?;
                let callback = self.signal_hook_value(signal, snapshot.kind)?;
                if is_nullish(callback) {
                    continue;
                }
                self.dispatch_property_callback(
                    NativeContinuation::signal_watcher_hook(
                        site,
                        Value::from_heap_ref(pending.raw()),
                        signal,
                    ),
                    callback,
                )?;
                return Ok(());
            }
            if let Some(signal) = snapshot.argument {
                self.pending_signal_watcher_advance_argument(pending)?;
                let mut hooks = Vec::new();
                hooks
                    .try_reserve(tuning::signals::INITIAL_OPERATION_CAPACITY)
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                match snapshot.kind {
                    SignalWatcherOperationKind::Watch => {
                        self.prepare_signal_watch(signal, snapshot.watcher, &mut hooks)?;
                    }
                    SignalWatcherOperationKind::Unwatch => {
                        self.prepare_signal_unwatch(signal, snapshot.watcher, &mut hooks)?;
                    }
                }
                self.pending_signal_watcher_append_hooks(pending, hooks)?;
                continue;
            }
            self.finish_signal_watcher_operation(site, snapshot.watcher, snapshot.kind)?;
            return Ok(());
        }
    }

    /// Continues a pending Watcher operation after one lifecycle callback returns.
    pub(crate) fn resume_signal_watcher_hook(
        &mut self,
        continuation: NativeContinuation,
    ) -> Result<(), ExecutionError> {
        let pending = self.pending_signal_watcher_reference(continuation.first())?;
        self.resume_signal_watcher_operation(continuation.site(), pending)
    }

    /// Copies only the next argument/hook so steady-state iteration allocates no scratch vectors.
    fn pending_signal_watcher_snapshot(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<SignalWatcherOperationSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow(pending, self.types.pending_signal_watcher_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(SignalWatcherOperationSnapshot {
                    watcher: node.watcher,
                    argument: node.arguments.get(node.argument_index).copied(),
                    hook: node.hooks.get(node.hook_index).copied(),
                    kind: node.kind,
                })
            })
        })
    }

    fn pending_signal_watcher_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingSignalWatcherOperation>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_signal_watcher_operation)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    fn pending_signal_watcher_advance_argument(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(pending, self.types.pending_signal_watcher_operation)
                    .map(|node| node.argument_index += 1)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn pending_signal_watcher_advance_hook(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(pending, self.types.pending_signal_watcher_operation)
                    .map(|node| node.hook_index += 1)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn pending_signal_watcher_append_hooks(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
        hooks: Vec<Value>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(pending, self.types.pending_signal_watcher_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                node.hooks
                    .try_reserve(hooks.len())
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                node.hooks.extend(hooks.iter().copied());
                Ok::<(), ExecutionError>(())
            })?;
            for hook in hooks {
                scope
                    .write_value_barrier(pending, hook)
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(())
        })
    }

    /// Returns the hook attached to one State, or `undefined` for Computed nodes.
    fn signal_hook_value(
        &mut self,
        signal: Value,
        kind: SignalWatcherOperationKind,
    ) -> Result<Value, ExecutionError> {
        let state = self.signal_state_reference(signal)?;
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow(state, self.types.signal_state)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(match kind {
                    SignalWatcherOperationKind::Watch => node.watched,
                    SignalWatcherOperationKind::Unwatch => node.unwatched,
                })
            })
        })
    }

    /// Applies one argument's graph transition and queues its first/last live hooks.
    fn prepare_signal_watch(
        &mut self,
        signal: Value,
        watcher: Value,
        hooks: &mut Vec<Value>,
    ) -> Result<(), ExecutionError> {
        let watcher_ref = self.signal_watcher_reference(watcher)?;
        let inserted = self.heap.with_running_scope(|scope| {
            let watcher = scope.root(watcher_ref).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(watcher, self.types.signal_watcher)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .watched
                    .insert(signal)
            })
        })?;
        if inserted {
            self.heap.with_running_scope(|scope| {
                let watcher = scope.root(watcher_ref).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(watcher, self.types.signal_watcher)
                        .map(|node| node.state = WatcherState::Watching)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            self.add_signal_sink(signal, watcher)?;
            self.attach_signal_liveness(signal, hooks)?;
        }
        Ok(())
    }

    /// Applies one argument's graph detach transition and queues its last-live hooks.
    fn prepare_signal_unwatch(
        &mut self,
        signal: Value,
        watcher: Value,
        hooks: &mut Vec<Value>,
    ) -> Result<(), ExecutionError> {
        let watcher_ref = self.signal_watcher_reference(watcher)?;
        let removed = self.heap.with_running_scope(|scope| {
            let watcher = scope.root(watcher_ref).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(watcher, self.types.signal_watcher)
                    .map_err(ExecutionError::NoGcBorrow)
                    .map(|node| node.watched.remove(signal))
            })
        })?;
        if removed {
            self.remove_signal_sink(signal, watcher)?;
            self.detach_signal_liveness(signal, hooks)?;
            self.heap.with_running_scope(|scope| {
                let watcher = scope.root(watcher_ref).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let node = no_gc
                        .borrow_mut(watcher, self.types.signal_watcher)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    node.pending.remove(signal);
                    if node.watched.entries.is_empty() {
                        node.state = WatcherState::Waiting;
                    }
                    Ok(())
                })
            })?;
        }
        Ok(())
    }

    fn finish_signal_watcher_operation(
        &mut self,
        site: NativeContinuationSite,
        watcher: Value,
        _kind: SignalWatcherOperationKind,
    ) -> Result<(), ExecutionError> {
        let watcher = self.signal_watcher_reference(watcher)?;
        self.heap.with_running_scope(|scope| {
            let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(watcher, self.types.signal_watcher)
                    .map(|node| {
                        if !node.watched.entries.is_empty() {
                            node.state = WatcherState::Watching;
                        }
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_immediate(Immediate::Undefined),
        )
    }

    /// Materializes the pending ordered subset as a fresh ordinary Array snapshot.
    pub(crate) fn signal_watcher_get_pending(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let watcher = self.signal_watcher_reference(site.this_value)?;
        let pending = self.heap.with_running_scope(|scope| {
            let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(watcher, self.types.signal_watcher)
                    .map_err(ExecutionError::NoGcBorrow)
                    .and_then(|node| node.pending.try_snapshot())
            })
        })?;
        let array = self.create_array_object_with_prototype(
            self.realm.array_prototype.expect("Array initialized"),
        )?;
        for (index, value) in pending.iter().copied().enumerate() {
            let key = self.property_key_atom(Value::from_i32(index as i32))?;
            self.set_own_data_property(array, key, value)?;
        }
        let length = self.intern_intrinsic_name(b"length")?;
        self.set_own_data_property(array, length, safe_integer_value(pending.len() as u64))?;
        Ok(array)
    }

    fn signal_prototype_for_new_target(
        &mut self,
        new_target: Value,
        kind: IntrinsicPrototypeKind,
        fallback: Value,
    ) -> Result<Value, ExecutionError> {
        if !self.is_object_value(new_target) {
            return Ok(fallback);
        }
        let prototype_atom = self.prototype_atom()?;
        let prototype = self
            .get_data_property(new_target, prototype_atom)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if self.is_object_value(prototype) {
            return Ok(prototype);
        }
        Ok(self
            .realm_for_callable(new_target)
            .ok()
            .and_then(|realm| self.realm_intrinsic_prototype(realm, kind))
            .unwrap_or(fallback))
    }

    fn signal_state_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<StateSignal>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        self.heap
            .checked_reference(raw, self.types.signal_state)
            .map_err(|_| ExecutionError::NotObject(value))
    }

    fn signal_computed_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<ComputedSignal>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        self.heap
            .checked_reference(raw, self.types.signal_computed)
            .map_err(|_| ExecutionError::NotObject(value))
    }

    fn signal_watcher_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<WatcherSignal>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        self.heap
            .checked_reference(raw, self.types.signal_watcher)
            .map_err(|_| ExecutionError::NotObject(value))
    }

    fn is_signal_value(&self, value: Value) -> bool {
        value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.signal_state)
                .is_ok()
                || self
                    .heap
                    .checked_reference(raw, self.types.signal_computed)
                    .is_ok()
        })
    }

    fn computed_snapshot(
        &mut self,
        computed: GcRef<ComputedSignal>,
    ) -> Result<(ComputedState, Vec<Value>, Value, Value), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let computed = scope.root(computed).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(computed, self.types.signal_computed)
                    .map_err(ExecutionError::NoGcBorrow)
                    .and_then(|node| {
                        Ok((
                            node.state,
                            node.sources.try_snapshot()?,
                            node.cached,
                            node.callback,
                        ))
                    })
            })
        })
    }

    fn set_computed_state(
        &mut self,
        computed: GcRef<ComputedSignal>,
        state: ComputedState,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let computed = scope.root(computed).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(computed, self.types.signal_computed)
                    .map(|node| node.state = state)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Rebuilds one Computed's ordered dependencies without retaining stale reverse edges.
    fn detach_computed_sources(
        &mut self,
        receiver: Value,
        computed: GcRef<ComputedSignal>,
        sources: Vec<Value>,
    ) -> Result<(), ExecutionError> {
        let live = self.heap.with_running_scope(|scope| {
            let computed = scope.root(computed).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(computed, self.types.signal_computed)
                    .map(|node| node.live_sinks != 0)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        for source in sources {
            if live {
                let mut ignored_hooks = Vec::new();
                ignored_hooks
                    .try_reserve(tuning::signals::INITIAL_OPERATION_CAPACITY)
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                self.detach_signal_liveness(source, &mut ignored_hooks)?;
            }
            self.remove_signal_sink(source, receiver)?;
        }
        self.heap.with_running_scope(|scope| {
            let computed = scope.root(computed).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(computed, self.types.signal_computed)
                    .map(|node| node.sources.entries.clear())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Adds a dependency in first-read order and publishes the reverse edge with a barrier.
    fn record_signal_dependency(&mut self, source: Value) -> Result<(), ExecutionError> {
        let Some(computing) = self.signal_runtime.computing else {
            return Ok(());
        };
        if computing == source {
            return Err(ExecutionError::NotObject(source));
        }
        let computed = self.signal_computed_reference(computing)?;
        let inserted = self.heap.with_running_scope(|scope| {
            let computed = scope.root(computed).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(computed, self.types.signal_computed)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .sources
                    .insert(source)
            })
        })?;
        if inserted {
            self.add_signal_sink(source, computing)?;
            let live = self.heap.with_running_scope(|scope| {
                let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(computed, self.types.signal_computed)
                        .map(|node| node.live_sinks != 0)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            if live {
                let mut ignored_hooks = Vec::new();
                ignored_hooks
                    .try_reserve(tuning::signals::INITIAL_OPERATION_CAPACITY)
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                self.attach_signal_liveness(source, &mut ignored_hooks)?;
            }
        }
        Ok(())
    }

    /// Adds one recursively live sink and queues first-live State hooks in source order.
    fn attach_signal_liveness(
        &mut self,
        source: Value,
        hooks: &mut Vec<Value>,
    ) -> Result<(), ExecutionError> {
        let mut work = Vec::new();
        work.try_reserve(tuning::signals::INITIAL_WORKLIST_CAPACITY)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        work.push(source);
        while let Some(current) = work.pop() {
            if let Ok(state) = self.signal_state_reference(current) {
                let (first, callback) = self.heap.with_running_scope(|scope| {
                    let state = scope.root(state).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        let node = no_gc
                            .borrow_mut(state, self.types.signal_state)
                            .map_err(ExecutionError::NoGcBorrow)?;
                        let first = node.live_sinks == 0;
                        node.live_sinks = node
                            .live_sinks
                            .checked_add(1)
                            .ok_or(ExecutionError::PropertyStorageAllocationFailed)?;
                        Ok((first, node.watched))
                    })
                })?;
                if first && !is_nullish(callback) {
                    hooks
                        .try_reserve(1)
                        .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                    hooks.push(current);
                }
                continue;
            }
            let computed = self.signal_computed_reference(current)?;
            let (first, sources) = self.heap.with_running_scope(|scope| {
                let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let node = no_gc
                        .borrow_mut(computed, self.types.signal_computed)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    let first = node.live_sinks == 0;
                    node.live_sinks = node
                        .live_sinks
                        .checked_add(1)
                        .ok_or(ExecutionError::PropertyStorageAllocationFailed)?;
                    Ok((first, node.sources.try_snapshot()?))
                })
            })?;
            if first {
                work.try_reserve(sources.len())
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                work.extend(sources.into_iter().rev());
            }
        }
        Ok(())
    }

    /// Removes one recursively live sink and queues last-live State hooks.
    fn detach_signal_liveness(
        &mut self,
        source: Value,
        hooks: &mut Vec<Value>,
    ) -> Result<(), ExecutionError> {
        let mut work = Vec::new();
        work.try_reserve(tuning::signals::INITIAL_WORKLIST_CAPACITY)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        work.push(source);
        while let Some(current) = work.pop() {
            if let Ok(state) = self.signal_state_reference(current) {
                let (last, callback) = self.heap.with_running_scope(|scope| {
                    let state = scope.root(state).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        let node = no_gc
                            .borrow_mut(state, self.types.signal_state)
                            .map_err(ExecutionError::NoGcBorrow)?;
                        if node.live_sinks == 0 {
                            return Err(ExecutionError::PropertyStorageAllocationFailed);
                        }
                        node.live_sinks -= 1;
                        Ok((node.live_sinks == 0, node.unwatched))
                    })
                })?;
                if last && !is_nullish(callback) {
                    hooks
                        .try_reserve(1)
                        .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                    hooks.push(current);
                }
                continue;
            }
            let computed = self.signal_computed_reference(current)?;
            let (last, sources) = self.heap.with_running_scope(|scope| {
                let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let node = no_gc
                        .borrow_mut(computed, self.types.signal_computed)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    if node.live_sinks == 0 {
                        return Err(ExecutionError::PropertyStorageAllocationFailed);
                    }
                    node.live_sinks -= 1;
                    Ok((node.live_sinks == 0, node.sources.try_snapshot()?))
                })
            })?;
            if last {
                work.try_reserve(sources.len())
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                work.extend(sources.into_iter().rev());
            }
        }
        Ok(())
    }

    fn add_signal_sink(&mut self, source: Value, sink: Value) -> Result<(), ExecutionError> {
        if let Ok(state) = self.signal_state_reference(source) {
            self.heap.with_running_scope(|scope| {
                let state = scope.root(state).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(state, self.types.signal_state)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .sinks
                        .insert(sink)
                        .map(|_| ())
                })
            })?;
        } else {
            let computed = self.signal_computed_reference(source)?;
            self.heap.with_running_scope(|scope| {
                let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(computed, self.types.signal_computed)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .sinks
                        .insert(sink)
                        .map(|_| ())
                })
            })?;
        }
        if let (Some(source), Some(sink)) = (source.as_heap_ref(), sink.as_heap_ref()) {
            self.heap
                .write_barrier(source, sink)
                .map_err(|_| ExecutionError::NotObject(Value::from_heap_ref(source)))?;
        }
        Ok(())
    }

    fn remove_signal_sink(&mut self, source: Value, sink: Value) -> Result<(), ExecutionError> {
        if let Ok(state) = self.signal_state_reference(source) {
            self.heap.with_running_scope(|scope| {
                let state = scope.root(state).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(state, self.types.signal_state)
                        .map(|node| node.sinks.remove(sink))
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
        } else {
            let computed = self.signal_computed_reference(source)?;
            self.heap.with_running_scope(|scope| {
                let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(computed, self.types.signal_computed)
                        .map(|node| node.sinks.remove(sink))
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
        }
        Ok(())
    }

    /// Performs ordered iterative propagation without using the Rust call stack.
    fn propagate_signal_change(&mut self, sinks: Vec<Value>) -> Result<(), ExecutionError> {
        self.signal_runtime.worklist.clear();
        self.signal_runtime
            .worklist
            .try_reserve(sinks.len().max(tuning::signals::INITIAL_WORKLIST_CAPACITY))
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        self.signal_runtime.worklist.extend(sinks.into_iter().rev());
        while let Some(sink) = self.signal_runtime.worklist.pop() {
            if let Ok(computed) = self.signal_computed_reference(sink) {
                let downstream = self.heap.with_running_scope(|scope| {
                    let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        let node = no_gc
                            .borrow_mut(computed, self.types.signal_computed)
                            .map_err(ExecutionError::NoGcBorrow)?;
                        node.state = ComputedState::Dirty;
                        node.sinks.try_snapshot()
                    })
                })?;
                self.signal_runtime
                    .worklist
                    .try_reserve(downstream.len())
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                self.signal_runtime
                    .worklist
                    .extend(downstream.into_iter().rev());
            } else {
                let watcher = self.signal_watcher_reference(sink)?;
                self.heap.with_running_scope(|scope| {
                    let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        let node = no_gc
                            .borrow_mut(watcher, self.types.signal_watcher)
                            .map_err(ExecutionError::NoGcBorrow)?;
                        let watched = node.watched.try_snapshot()?;
                        for watched in watched {
                            node.pending.insert(watched)?;
                        }
                        node.state = WatcherState::Pending;
                        Ok(())
                    })
                })?;
            }
        }
        Ok(())
    }

    /// Validates the whole watch/unwatch list before any graph mutation occurs.
    fn validated_signal_arguments(
        &mut self,
        site: &CallSite,
        require_watched: bool,
    ) -> Result<Vec<Value>, ExecutionError> {
        let watcher = self.signal_watcher_reference(site.this_value)?;
        let watched = self.heap.with_running_scope(|scope| {
            let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(watcher, self.types.signal_watcher)
                    .map_err(ExecutionError::NoGcBorrow)
                    .and_then(|node| node.watched.try_snapshot())
            })
        })?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(site.argument_count as usize)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        for index in 0..site.argument_count {
            let value = self
                .call_argument(site, index)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            if !self.is_signal_value(value) || (require_watched && !watched.contains(&value)) {
                return Err(ExecutionError::NotObject(value));
            }
            values.push(value);
        }
        Ok(values)
    }
}
