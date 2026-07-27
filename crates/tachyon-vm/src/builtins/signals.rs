//! Native TC39 Signals graph payloads and the first executable API slice.

use super::super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ComputedState {
    Clean,
    Checked,
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

const COMPUTED_UNINITIALIZED_GENERATION: u64 = u64::MAX;
const COMPUTED_THROW_COMPLETION_BIT: u64 = 1 << 63;
const COMPUTED_GENERATION_MASK: u64 = !COMPUTED_THROW_COMPLETION_BIT;
const COMPUTED_CALLBACK_SLOT: usize = 0;
const COMPUTED_EQUALS_SLOT: usize = 1;
const COMPUTED_WATCHED_SLOT: usize = 2;
const COMPUTED_UNWATCHED_SLOT: usize = 3;
const COMPUTED_OPTIONS_EQUALS_SLOT: usize = 2;
const COMPUTED_OPTIONS_WATCHED_SLOT: usize = 3;
const COMPUTED_OPTIONS_UNWATCHED_SLOT: usize = 4;
const COMPUTED_OPTIONS_RECORD_COUNT: u8 = 5;
const SIGNAL_OPTIONS_WATCHED_KEY_SLOT: usize = 3;
const SIGNAL_OPTIONS_UNWATCHED_KEY_SLOT: usize = 4;

#[derive(Clone, Copy, Debug)]
struct SignalComputedCallbacks {
    callback: Value,
    equals: Value,
    watched: Value,
    unwatched: Value,
}

#[inline(always)]
const fn computed_generation_is_throw(generation: u64) -> bool {
    generation != COMPUTED_UNINITIALIZED_GENERATION
        && generation & COMPUTED_THROW_COMPLETION_BIT != 0
}

#[inline(always)]
fn signal_previous_computing(value: Value) -> Option<Value> {
    (value.as_immediate() != Some(Immediate::Undefined)).then_some(value)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum SignalWatcherOperationKind {
    Watch,
    Unwatch,
    ComputedPull,
    ComputedEquals,
    Notify,
}

#[derive(Clone, Copy, Debug)]
struct SignalComputedPullFrame {
    computed: Value,
    next_source: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum SignalLifecycleHookKind {
    Watched,
    Unwatched,
    Pull,
}

#[derive(Clone, Copy, Debug)]
struct SignalLifecycleHook {
    signal: Value,
    kind: SignalLifecycleHookKind,
    next_source: u32,
}

impl Trace for SignalLifecycleHook {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.signal.trace(tracer);
    }
}

/// Traced arguments and hook queue for one resumable Watcher mutation.
#[derive(Debug)]
pub(crate) struct PendingSignalWatcherOperation {
    watcher: Value,
    arguments: Vec<Value>,
    hooks: Vec<SignalLifecycleHook>,
    argument_index: usize,
    hook_index: usize,
    kind: SignalWatcherOperationKind,
}

#[derive(Debug)]
struct SignalWatcherOperationSnapshot {
    watcher: Value,
    argument: Option<Value>,
    hook: Option<SignalLifecycleHook>,
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

struct SignalWatcherAllocationRoots<'a> {
    vm: VmRoots<'a>,
    watcher: Value,
    arguments: Vec<Value>,
}

impl Trace for SignalWatcherAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.watcher.trace(tracer);
        self.arguments.trace(tracer);
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

struct SignalIntrospectionRoots<'a> {
    vm: VmRoots<'a>,
    prototype: Value,
    entries: Vec<Value>,
    storage: Option<GcRef<PropertyStorage>>,
    elements: Option<GcRef<ArrayElements>>,
}

impl Trace for SignalIntrospectionRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.prototype.trace(tracer);
        self.entries.trace(tracer);
        self.storage.trace(tracer);
        self.elements.trace(tracer);
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
    pub(crate) frozen: bool,
    pub(crate) generation: u64,
    worklist: Vec<Value>,
}

impl Isolate {
    /// Rolls back agent-wide Signal state before a terminal host error discards the active Fiber.
    pub(crate) fn cancel_signal_execution(&mut self) -> Result<(), ExecutionError> {
        for index in (0..self.fiber.completions.len()).rev() {
            let Some(continuation) = self.fiber.completions.native_at(index) else {
                continue;
            };
            match continuation.kind() {
                NativeContinuationKind::SignalWatcherHook => {
                    let pending = self.pending_signal_watcher_reference(continuation.first())?;
                    if self.pending_signal_watcher_kind(pending)?
                        == SignalWatcherOperationKind::Notify
                    {
                        self.set_signal_watcher_waiting(continuation.second())?;
                    }
                    self.signal_runtime.frozen = false;
                }
                NativeContinuationKind::SignalComputed => {
                    let pending = self.pending_signal_watcher_reference(continuation.first())?;
                    let receiver = self
                        .pending_signal_computed_pull_top(pending)?
                        .ok_or(ExecutionError::MissingNativeContinuation)?
                        .computed;
                    let computed = self.signal_computed_reference(receiver)?;
                    self.restore_failed_signal_computed_start(
                        computed,
                        pending,
                        signal_previous_computing(continuation.second()),
                    )?;
                }
                NativeContinuationKind::SignalUntrack => {
                    self.restore_signal_untrack_owner(continuation);
                }
                _ => {}
            }
        }
        self.signal_runtime.computing = None;
        self.signal_runtime.frozen = false;
        Ok(())
    }

    /// Returns the current dependency owner without exposing its graph payload.
    #[inline(always)]
    pub(crate) fn signal_current_computed(&self) -> Value {
        self.signal_runtime
            .computing
            .unwrap_or(Value::from_immediate(Immediate::Undefined))
    }

    /// Returns a fresh ordered snapshot of the Computed or Watcher source edges.
    pub(crate) fn signal_introspect_sources(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let subject = self.signal_introspection_argument(site)?;
        let entries = if let Ok(computed) = self.signal_computed_reference(subject) {
            self.heap.with_running_scope(|scope| {
                let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(computed, self.types.signal_computed)
                        .map_err(ExecutionError::NoGcBorrow)
                        .and_then(|node| node.sources.try_snapshot())
                })
            })?
        } else if let Ok(watcher) = self.signal_watcher_reference(subject) {
            self.heap.with_running_scope(|scope| {
                let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(watcher, self.types.signal_watcher)
                        .map_err(ExecutionError::NoGcBorrow)
                        .and_then(|node| node.watched.try_snapshot())
                })
            })?
        } else {
            return Err(ExecutionError::NotObject(subject));
        };
        self.publish_signal_introspection_snapshot(site, entries)
    }

    /// Returns a fresh ordered snapshot of the live State or Computed sink edges.
    pub(crate) fn signal_introspect_sinks(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let subject = self.signal_introspection_argument(site)?;
        let entries = if let Ok(state) = self.signal_state_reference(subject) {
            self.heap.with_running_scope(|scope| {
                let state = scope.root(state).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(state, self.types.signal_state)
                        .map_err(ExecutionError::NoGcBorrow)
                        .and_then(|node| node.sinks.try_snapshot())
                })
            })?
        } else if let Ok(computed) = self.signal_computed_reference(subject) {
            self.heap.with_running_scope(|scope| {
                let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(computed, self.types.signal_computed)
                        .map_err(ExecutionError::NoGcBorrow)
                        .and_then(|node| node.sinks.try_snapshot())
                })
            })?
        } else {
            return Err(ExecutionError::NotObject(subject));
        };
        let entries = self.filter_live_signal_sinks(entries)?;
        self.publish_signal_introspection_snapshot(site, entries)
    }

    /// Reports whether a Computed or Watcher currently retains any ordered source edge.
    pub(crate) fn signal_has_sources(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let subject = self.signal_introspection_argument(site)?;
        let has_sources = if let Ok(computed) = self.signal_computed_reference(subject) {
            self.heap.with_running_scope(|scope| {
                let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(computed, self.types.signal_computed)
                        .map(|node| !node.sources.entries.is_empty())
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?
        } else if let Ok(watcher) = self.signal_watcher_reference(subject) {
            self.heap.with_running_scope(|scope| {
                let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(watcher, self.types.signal_watcher)
                        .map(|node| !node.watched.entries.is_empty())
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?
        } else {
            return Err(ExecutionError::NotObject(subject));
        };
        Ok(Value::from_immediate(if has_sources {
            Immediate::True
        } else {
            Immediate::False
        }))
    }

    /// Reports whether a State or Computed currently has any recursively live sink edge.
    pub(crate) fn signal_has_sinks(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let subject = self.signal_introspection_argument(site)?;
        let has_sinks = if let Ok(state) = self.signal_state_reference(subject) {
            self.heap.with_running_scope(|scope| {
                let state = scope.root(state).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(state, self.types.signal_state)
                        .map(|node| node.live_sinks != 0)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?
        } else if let Ok(computed) = self.signal_computed_reference(subject) {
            self.heap.with_running_scope(|scope| {
                let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(computed, self.types.signal_computed)
                        .map(|node| node.live_sinks != 0)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?
        } else {
            return Err(ExecutionError::NotObject(subject));
        };
        Ok(Value::from_immediate(if has_sinks {
            Immediate::True
        } else {
            Immediate::False
        }))
    }

    /// Removes cold Computed dependency-index edges while preserving live sink insertion order.
    fn filter_live_signal_sinks(
        &mut self,
        mut entries: Vec<Value>,
    ) -> Result<Vec<Value>, ExecutionError> {
        let mut write = 0;
        for read in 0..entries.len() {
            let sink = entries[read];
            let live = if self.signal_watcher_reference(sink).is_ok() {
                true
            } else {
                let computed = self.signal_computed_reference(sink)?;
                self.heap.with_running_scope(|scope| {
                    let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        no_gc
                            .borrow(computed, self.types.signal_computed)
                            .map(|node| node.live_sinks != 0)
                            .map_err(ExecutionError::NoGcBorrow)
                    })
                })?
            };
            if live {
                entries[write] = sink;
                write += 1;
            }
        }
        entries.truncate(write);
        Ok(entries)
    }

    #[inline(always)]
    fn signal_introspection_argument(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        self.ensure_signal_runtime_unfrozen(site.this_value)?;
        self.call_argument(site, 0)
            .map(|argument| argument.unwrap_or(Value::from_immediate(Immediate::Undefined)))
    }

    /// Publishes the result before property definitions so every later GC sees the Array root.
    fn publish_signal_introspection_snapshot(
        &mut self,
        site: &CallSite,
        entries: Vec<Value>,
    ) -> Result<(), ExecutionError> {
        let length = self.length_atom()?;
        let shape = self
            .shapes
            .transition_add(
                ShapeId::EMPTY,
                length,
                PropertyAttributes::data(true, false, false),
            )
            .map_err(ExecutionError::Shape)?;
        let mut elements = ArrayElements::with_capacity(entries.len())
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        for (index, value) in entries.iter().copied().enumerate() {
            let index = u32::try_from(index).map_err(|_| ExecutionError::InvalidArrayLength)?;
            elements
                .set(index, value)
                .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        }
        let prototype = self.realm.array_prototype.expect("Array initialized");
        let mut roots = SignalIntrospectionRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            prototype,
            entries,
            storage: None,
            elements: None,
        };
        let storage = self
            .heap
            .try_allocate_external_with_gc(
                self.types.property_storage,
                0,
                PropertyStorage::new(Box::new([safe_integer_value(roots.entries.len() as u64)])),
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        roots.storage = Some(storage);
        let elements = self
            .heap
            .try_allocate_external_with_gc(
                self.types.array_elements,
                0,
                elements,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        roots.elements = Some(elements);
        let array = self
            .heap
            .try_allocate_with_gc(
                self.types.array,
                0,
                0,
                ArrayObject {
                    ordinary: OrdinaryObject {
                        shape,
                        extensible: true,
                        storage: roots.storage,
                        prototype: roots.prototype,
                    },
                    elements: roots.elements,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(array.raw()),
        )
    }

    /// Calls one callback with dependency ownership suspended until normal or abrupt completion.
    pub(crate) fn begin_signal_untrack(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.ensure_signal_runtime_unfrozen(site.this_value)?;
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_callable_value(callback)? {
            return Err(ExecutionError::NonCallable(callback));
        }
        let previous = self
            .signal_runtime
            .computing
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.fiber
            .completions
            .push_native(NativeContinuation::signal_untrack(
                NativeContinuationSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    call_site: site.call_site,
                },
                previous,
            ))
            .map_err(Self::completion_stack_error)?;
        self.signal_runtime.computing = None;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: callback,
            argument_base: 0,
            argument_source: None,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 0,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        }) {
            let continuation = self.pop_native_continuation()?;
            self.restore_signal_untrack_owner(continuation);
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("untrack callback publishes one callee frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_signal_untrack(continuation, value)
    }

    /// Restores dependency ownership and forwards a normal callback result unchanged.
    pub(crate) fn resume_signal_untrack(
        &mut self,
        continuation: NativeContinuation,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.restore_signal_untrack_owner(continuation);
        self.write(
            continuation.site().caller_base,
            continuation.site().destination,
            value,
        )
    }

    /// Restores dependency ownership while the original thrown completion keeps unwinding.
    pub(crate) fn continue_signal_untrack_abrupt(&mut self, continuation: NativeContinuation) {
        self.restore_signal_untrack_owner(continuation);
    }

    #[inline(always)]
    fn restore_signal_untrack_owner(&mut self, continuation: NativeContinuation) {
        self.signal_runtime.computing = signal_previous_computing(continuation.first());
    }

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
        let (watched_symbol, unwatched_symbol) = self.signal_option_symbols(site.callee)?;
        let pending = self.allocate_signal_state_call_state(NativeCallState {
            values: [
                state,
                options,
                Value::from_immediate(Immediate::Undefined),
                watched_symbol,
                unwatched_symbol,
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
        if stage == SignalStateStage::ComputedOptionsEquals {
            if value.as_immediate() != Some(Immediate::Undefined)
                && !self.is_callable_value(value)?
            {
                return Err(ExecutionError::NonCallable(value));
            }
            self.set_signal_computed_option(pending, COMPUTED_OPTIONS_EQUALS_SLOT, value)?;
            return self.dispatch_signal_state_option_get(
                continuation.site(),
                pending,
                SignalStateStage::OptionsWatched,
            );
        }
        let computed_options = snapshot.count == COMPUTED_OPTIONS_RECORD_COUNT;
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
                if computed_options {
                    self.set_signal_computed_option(pending, COMPUTED_OPTIONS_WATCHED_SLOT, value)?;
                    return self.dispatch_signal_state_option_get(
                        continuation.site(),
                        pending,
                        SignalStateStage::OptionsUnwatched,
                    );
                }
                self.set_signal_state_option(state_value, stage, value)?;
                self.dispatch_signal_state_option_get(
                    continuation.site(),
                    pending,
                    SignalStateStage::OptionsUnwatched,
                )
            }
            SignalStateStage::OptionsUnwatched => {
                if computed_options {
                    self.set_signal_computed_option(
                        pending,
                        COMPUTED_OPTIONS_UNWATCHED_SLOT,
                        value,
                    )?;
                    let snapshot = self.native_call_state_snapshot(pending)?;
                    return self.finish_signal_computed_options(
                        continuation.site(),
                        snapshot.values[0],
                        snapshot.values[COMPUTED_OPTIONS_EQUALS_SLOT],
                        snapshot.values[COMPUTED_OPTIONS_WATCHED_SLOT],
                        snapshot.values[COMPUTED_OPTIONS_UNWATCHED_SLOT],
                    );
                }
                self.set_signal_state_option(state_value, stage, value)?;
                self.write(
                    continuation.site().caller_base,
                    continuation.site().destination,
                    state_value,
                )
            }
            SignalStateStage::ComputedOptionsEquals => {
                unreachable!("Computed options resume before State dispatch")
            }
            SignalStateStage::Equals => {
                if !self.is_truthy_value(value)? {
                    self.commit_signal_state_value(
                        continuation.site(),
                        state_value,
                        snapshot.values[1],
                    )?;
                    return Ok(());
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
            SignalStateStage::ComputedOptionsEquals => {
                self.intern_intrinsic_name(b"equals")?.into()
            }
            SignalStateStage::OptionsWatched => {
                self.property_key(snapshot.values[SIGNAL_OPTIONS_WATCHED_KEY_SLOT])?
            }
            SignalStateStage::OptionsUnwatched => {
                self.property_key(snapshot.values[SIGNAL_OPTIONS_UNWATCHED_KEY_SLOT])?
            }
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
                    SignalStateStage::ComputedOptionsEquals => {
                        return Err(ExecutionError::MissingNativeContinuation);
                    }
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

    /// Validates callback/options order and begins the observable Computed options read.
    pub(crate) fn begin_signal_computed_constructor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_callable_value(callback)? {
            return Err(ExecutionError::NonCallable(callback));
        }
        let options = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let prototype = self.signal_prototype_for_new_target(
            site.new_target,
            IntrinsicPrototypeKind::SignalComputed,
            self.realm
                .signal_computed_prototype
                .expect("Signal.Computed initialized"),
        )?;
        let computed = self.allocate_signal_computed(callback, prototype)?;
        if is_nullish(options) {
            return self.write(site.caller_base, site.destination, computed);
        }
        let (watched_symbol, unwatched_symbol) = self.signal_option_symbols(site.callee)?;
        let pending = self.allocate_signal_state_call_state(NativeCallState {
            values: [
                computed,
                options,
                Value::from_immediate(Immediate::Undefined),
                watched_symbol,
                unwatched_symbol,
            ],
            count: COMPUTED_OPTIONS_RECORD_COUNT,
        })?;
        self.dispatch_signal_state_option_get(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            pending,
            SignalStateStage::ComputedOptionsEquals,
        )
    }

    /// Allocates one initially-dirty Computed without changing its resident layout.
    fn allocate_signal_computed(
        &mut self,
        callback: Value,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
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
                    generation: COMPUTED_UNINITIALIZED_GENERATION,
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

    /// Installs one cold callback/options sidecar only when Computed has custom behavior.
    fn finish_signal_computed_options(
        &mut self,
        site: NativeContinuationSite,
        computed: Value,
        equals: Value,
        watched: Value,
        unwatched: Value,
    ) -> Result<(), ExecutionError> {
        if equals.as_immediate() == Some(Immediate::Undefined)
            && is_nullish(watched)
            && is_nullish(unwatched)
        {
            return self.write(site.caller_base, site.destination, computed);
        }
        self.write(site.caller_base, site.destination, computed)?;
        let computed_ref = self.signal_computed_reference(computed)?;
        let callback = self.computed_snapshot(computed_ref)?.3;
        let sidecar = self.allocate_signal_state_call_state(NativeCallState {
            values: [
                callback,
                equals,
                watched,
                unwatched,
                Value::from_immediate(Immediate::Undefined),
            ],
            count: 4,
        })?;
        let sidecar = Value::from_heap_ref(sidecar.raw());
        self.heap.with_running_scope(|scope| {
            let computed_ref = scope.root(computed_ref).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(computed_ref, self.types.signal_computed)
                    .map(|node| node.callback = sidecar)
                    .map_err(ExecutionError::NoGcBorrow)
            })?;
            scope
                .write_value_barrier(computed_ref, sidecar)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })?;
        self.write(site.caller_base, site.destination, computed)
    }

    /// Publishes one Computed option into the traced constructor record before the next Get.
    fn set_signal_computed_option(
        &mut self,
        pending: GcRef<NativeCallState>,
        index: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(pending, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let slot = state
                    .values
                    .get_mut(index)
                    .ok_or(ExecutionError::MissingNativeContinuation)?;
                *slot = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(pending, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Allocates one Watcher node; notification dispatch is added by the next graph slice.
    pub(crate) fn create_signal_watcher_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let notify = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_callable_value(notify)? {
            return Err(ExecutionError::NonCallable(notify));
        }
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
        self.ensure_signal_runtime_unfrozen(receiver)?;
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
        self.ensure_signal_runtime_unfrozen(receiver)?;
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
                self.commit_signal_state_value(
                    NativeContinuationSite {
                        caller_base: site.caller_base,
                        destination: site.destination,
                        call_site: site.call_site,
                    },
                    receiver,
                    value,
                )?;
                return Ok(());
            }
            return self.write(
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::Undefined),
            );
        }
        if !self.is_callable_value(equals)? {
            return Err(ExecutionError::NonCallable(equals));
        }
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
        site: NativeContinuationSite,
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
        let watchers = self.propagate_signal_change(sinks)?;
        self.begin_signal_watcher_notifications(site, watchers)
    }

    /// Allocates a traced notify queue after graph coloring and drains it synchronously.
    fn begin_signal_watcher_notifications(
        &mut self,
        site: NativeContinuationSite,
        watchers: Vec<Value>,
    ) -> Result<(), ExecutionError> {
        if watchers.is_empty() {
            return self.write(
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::Undefined),
            );
        }
        let pending = self.allocate_pending_signal_watcher_operation(
            Value::from_immediate(Immediate::Undefined),
            SignalWatcherOperationKind::Notify,
            watchers,
        )?;
        self.resume_signal_watcher_operation(site, pending)
    }

    /// Rethrows one notify failure by identity or creates the ordered AggregateError result.
    fn finish_signal_watcher_notifications(
        &mut self,
        site: NativeContinuationSite,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<(), ExecutionError> {
        match self.signal_watcher_notification_error(site, pending)? {
            None => self.write(
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::Undefined),
            ),
            Some(error) => Err(ExecutionError::HostThrown(error)),
        }
    }

    /// Returns the rooted single error or constructs an AggregateError for multiple failures.
    fn signal_watcher_notification_error(
        &mut self,
        site: NativeContinuationSite,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<Option<Value>, ExecutionError> {
        let error_count = self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow(pending, self.types.pending_signal_watcher_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(node.arguments.len().saturating_sub(node.hook_index))
            })
        })?;
        match error_count {
            0 => Ok(None),
            1 => self
                .pending_signal_watcher_notify_error(pending, 0)
                .map(Some),
            count => {
                self.write(
                    site.caller_base,
                    site.destination,
                    Value::from_heap_ref(pending.raw()),
                )?;
                let array = self.create_array_object_with_prototype(
                    self.realm.array_prototype.expect("Array initialized"),
                )?;
                self.append_signal_watcher_notify_error(pending, array)?;
                for index in 0..count {
                    let key = self.property_key_atom(Value::from_i32(index as i32))?;
                    let error = self.pending_signal_watcher_notify_error(pending, index)?;
                    self.set_own_data_property(array, key, error)?;
                }
                let aggregate = self.create_native_error(NativeErrorKind::Aggregate, None)?;
                self.append_signal_watcher_notify_error(pending, aggregate)?;
                let errors_atom = self.intern_intrinsic_name(b"errors")?;
                self.define_data_property(
                    aggregate,
                    errors_atom,
                    DataPropertyDescriptor {
                        value: Some(array),
                        writable: Some(true),
                        enumerable: Some(false),
                        configurable: Some(true),
                    },
                )?;
                Ok(Some(aggregate))
            }
        }
    }

    fn pending_signal_watcher_notify_error(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
        index: usize,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(pending, self.types.pending_signal_watcher_operation)
                    .map_err(ExecutionError::NoGcBorrow)
                    .and_then(|node| {
                        node.arguments
                            .get(node.hook_index + index)
                            .copied()
                            .ok_or(ExecutionError::MissingNativeContinuation)
                    })
            })
        })
    }

    /// Returns a cached Computed value or dispatches its callback through a native continuation.
    pub(crate) fn begin_signal_computed_get(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        self.ensure_signal_runtime_unfrozen(receiver)?;
        let computed = self.signal_computed_reference(receiver)?;
        let snapshot = self.computed_snapshot(computed)?;
        if snapshot.0 == ComputedState::Clean {
            self.record_signal_dependency(receiver)?;
            if computed_generation_is_throw(snapshot.4) {
                return Err(ExecutionError::HostThrown(snapshot.2));
            }
            return self.write(site.caller_base, site.destination, snapshot.2);
        }
        if snapshot.0 == ComputedState::Computing {
            let error = self.create_native_error(NativeErrorKind::Type, None)?;
            self.commit_signal_computed_completion(computed, error, true)?;
            return Err(ExecutionError::HostThrown(error));
        }
        let pending = self.allocate_pending_signal_computed_pull(receiver)?;
        self.resume_signal_computed_pull(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            pending,
        )
    }

    /// Polls a Checked graph iteratively and starts only the deepest dirty callback.
    fn resume_signal_computed_pull(
        &mut self,
        site: NativeContinuationSite,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<(), ExecutionError> {
        loop {
            let Some(frame) = self.pending_signal_computed_pull_top(pending)? else {
                return self.finish_signal_computed_pull(site, pending);
            };
            let computed = self.signal_computed_reference(frame.computed)?;
            let snapshot = self.computed_pull_snapshot(computed, frame.next_source)?;
            match snapshot.0 {
                ComputedState::Clean => {
                    self.pending_signal_computed_pull_pop(pending)?;
                }
                ComputedState::Computing => {
                    let cycle = self.signal_computed_reference(frame.computed)?;
                    let error = self.create_native_error(NativeErrorKind::Type, None)?;
                    self.commit_signal_computed_completion(cycle, error, true)?;
                    return Err(ExecutionError::HostThrown(error));
                }
                ComputedState::Dirty => {
                    return self.start_signal_computed_callback(site, pending, frame.computed);
                }
                ComputedState::Checked => {
                    if let Some(source) = snapshot.1 {
                        self.pending_signal_computed_pull_advance(pending)?;
                        if let Ok(source) = self.signal_computed_reference(source) {
                            let state = self.computed_pull_snapshot(source, 0)?.0;
                            if state != ComputedState::Clean {
                                self.pending_signal_computed_pull_push(
                                    pending,
                                    Value::from_heap_ref(source.raw()),
                                )?;
                            }
                        }
                        continue;
                    }
                    self.set_computed_state(computed, ComputedState::Clean)?;
                    self.clear_signal_computed_from_watcher_pending(frame.computed)?;
                    self.pending_signal_computed_pull_pop(pending)?;
                }
            }
        }
    }

    /// Publishes old sources, enters Computing, and dispatches one callback without Rust recursion.
    fn start_signal_computed_callback(
        &mut self,
        site: NativeContinuationSite,
        pending: GcRef<PendingSignalWatcherOperation>,
        receiver: Value,
    ) -> Result<(), ExecutionError> {
        let computed = self.signal_computed_reference(receiver)?;
        let snapshot = self.computed_snapshot(computed)?;
        let callback = self.signal_computed_callbacks(snapshot.3)?.callback;
        self.set_pending_signal_watcher_arguments(pending, snapshot.1)?;
        self.clear_computed_sources(computed)?;
        let previous = self.signal_runtime.computing.replace(receiver);
        self.set_computed_state(computed, ComputedState::Computing)?;
        if self
            .fiber
            .completions
            .push_native(NativeContinuation::signal_computed(
                site,
                Value::from_heap_ref(pending.raw()),
                previous.unwrap_or(Value::from_immediate(Immediate::Undefined)),
            ))
            .is_err()
        {
            self.restore_failed_signal_computed_start(computed, pending, previous)?;
            return Err(ExecutionError::CompletionAllocationFailed);
        }
        let frame_depth = self.fiber.frames.len();
        let result = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: callback,
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
            let continuation = self.pop_native_continuation()?;
            if let ExecutionError::HostThrown(thrown) = error {
                return match self.continue_signal_computed_abrupt(continuation, thrown)? {
                    Some(error) => Err(ExecutionError::HostThrown(error)),
                    None => Ok(()),
                };
            }
            self.restore_failed_signal_computed_start(computed, pending, previous)?;
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

    /// Restores old dependencies when a callback cannot enter the resumable JS-frame path.
    fn restore_failed_signal_computed_start(
        &mut self,
        computed: GcRef<ComputedSignal>,
        pending: GcRef<PendingSignalWatcherOperation>,
        previous: Option<Value>,
    ) -> Result<(), ExecutionError> {
        let old_sources = self.pending_signal_computed_old_sources(pending)?;
        self.signal_runtime.computing = previous;
        self.heap.with_running_scope(|scope| {
            let computed = scope.root(computed).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(computed, self.types.signal_computed)
                    .map_err(ExecutionError::NoGcBorrow)?;
                node.sources.entries.clear();
                node.sources
                    .entries
                    .try_reserve(old_sources.len())
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                node.sources.entries.extend(old_sources.iter().copied());
                node.state = ComputedState::Dirty;
                Ok::<(), ExecutionError>(())
            })?;
            for source in old_sources {
                scope
                    .write_value_barrier(computed, source)
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(())
        })
    }

    /// Commits one successful callback and resumes its iterative pull operation.
    pub(crate) fn resume_signal_computed(
        &mut self,
        continuation: NativeContinuation,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.pending_signal_watcher_reference(continuation.first())?;
        self.write(
            continuation.site().caller_base,
            continuation.site().destination,
            Value::from_heap_ref(pending.raw()),
        )?;
        if self.pending_signal_watcher_kind(pending)? == SignalWatcherOperationKind::ComputedEquals
        {
            return self.finish_signal_computed_equals(continuation, pending, value);
        }
        let receiver = self
            .pending_signal_computed_pull_top(pending)?
            .ok_or(ExecutionError::MissingNativeContinuation)?
            .computed;
        let computed = self.signal_computed_reference(receiver)?;
        let snapshot = self.computed_snapshot(computed)?;
        let old = snapshot.2;
        let initialized = snapshot.4 != COMPUTED_UNINITIALIZED_GENERATION
            && !computed_generation_is_throw(snapshot.4);
        let equals = self.signal_computed_callbacks(snapshot.3)?.equals;
        if initialized && equals.as_immediate() != Some(Immediate::Undefined) {
            return self.begin_signal_computed_equals(
                continuation,
                pending,
                receiver,
                old,
                value,
                equals,
            );
        }
        self.commit_signal_computed_completion(computed, value, false)?;
        let changed = !initialized || !self.same_value(old, value)?;
        self.finish_signal_computed_recompute(continuation, pending, receiver, computed, changed)
    }

    /// Calls a custom comparator while the recomputed signal remains the active dependency owner.
    fn begin_signal_computed_equals(
        &mut self,
        continuation: NativeContinuation,
        pending: GcRef<PendingSignalWatcherOperation>,
        receiver: Value,
        old: Value,
        new: Value,
        equals: Value,
    ) -> Result<(), ExecutionError> {
        let arguments = match self.allocate_signal_state_call_state(NativeCallState {
            values: [
                old,
                new,
                Value::from_immediate(Immediate::Undefined),
                Value::from_immediate(Immediate::Undefined),
                Value::from_immediate(Immediate::Undefined),
            ],
            count: 2,
        }) {
            Ok(arguments) => arguments,
            Err(error) => {
                let computed = self.signal_computed_reference(receiver)?;
                self.restore_failed_signal_computed_start(
                    computed,
                    pending,
                    signal_previous_computing(continuation.second()),
                )?;
                return Err(error);
            }
        };
        if let Err(error) = self.prepare_pending_signal_computed_equals(pending, arguments) {
            let computed = self.signal_computed_reference(receiver)?;
            self.restore_failed_signal_computed_start(
                computed,
                pending,
                signal_previous_computing(continuation.second()),
            )?;
            return Err(error);
        }
        let prefix = match self.create_apply_argument_prefix(equals, receiver, vec![old, new]) {
            Ok(prefix) => prefix,
            Err(error) => {
                let computed = self.signal_computed_reference(receiver)?;
                self.restore_failed_signal_computed_start(
                    computed,
                    pending,
                    signal_previous_computing(continuation.second()),
                )?;
                return Err(error);
            }
        };
        if self
            .fiber
            .completions
            .push_native(NativeContinuation::signal_computed(
                continuation.site(),
                Value::from_heap_ref(pending.raw()),
                continuation.second(),
            ))
            .is_err()
        {
            let computed = self.signal_computed_reference(receiver)?;
            self.restore_failed_signal_computed_start(
                computed,
                pending,
                signal_previous_computing(continuation.second()),
            )?;
            return Err(ExecutionError::CompletionAllocationFailed);
        }
        let frame_depth = self.fiber.frames.len();
        let result = self.call(CallSite {
            caller_base: continuation.site().caller_base,
            destination: continuation.site().destination,
            callee: equals,
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: 2,
            argument_count: 2,
            this_value: receiver,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: continuation.site().call_site,
        });
        if let Err(error) = result {
            let continuation = self.pop_native_continuation()?;
            if let ExecutionError::HostThrown(thrown) = error {
                return match self.continue_signal_computed_abrupt(continuation, thrown)? {
                    Some(error) => Err(ExecutionError::HostThrown(error)),
                    None => Ok(()),
                };
            }
            let computed = self.signal_computed_reference(receiver)?;
            self.restore_failed_signal_computed_start(
                computed,
                pending,
                signal_previous_computing(continuation.second()),
            )?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("custom equals callback frame was pushed");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let returned = self.read(
            continuation.site().caller_base,
            continuation.site().destination,
        )?;
        self.resume_signal_computed(continuation, returned)
    }

    /// Commits a custom equals result and preserves equals-read dependencies on this Computed.
    fn finish_signal_computed_equals(
        &mut self,
        continuation: NativeContinuation,
        pending: GcRef<PendingSignalWatcherOperation>,
        result: Value,
    ) -> Result<(), ExecutionError> {
        let receiver = self
            .pending_signal_computed_pull_top(pending)?
            .ok_or(ExecutionError::MissingNativeContinuation)?
            .computed;
        let computed = self.signal_computed_reference(receiver)?;
        let arguments = self.pending_signal_computed_equals_arguments(pending)?;
        let equal = self.is_truthy_value(result)?;
        let cached = if equal { arguments.0 } else { arguments.1 };
        self.commit_signal_computed_completion(computed, cached, false)?;
        self.finish_signal_computed_recompute(continuation, pending, receiver, computed, !equal)
    }

    /// Stores a normal or abrupt cache entry and publishes its generational edge.
    fn commit_signal_computed_completion(
        &mut self,
        computed: GcRef<ComputedSignal>,
        value: Value,
        thrown: bool,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let computed = scope.root(computed).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(computed, self.types.signal_computed)
                    .map_err(ExecutionError::NoGcBorrow)?;
                node.cached = value;
                node.state = ComputedState::Clean;
                node.generation = (self.signal_runtime.generation & COMPUTED_GENERATION_MASK)
                    | if thrown {
                        COMPUTED_THROW_COMPLETION_BIT
                    } else {
                        0
                    };
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(computed, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Reconciles dependencies, restores the outer owner, and resumes the remaining pull stack.
    fn finish_signal_computed_recompute(
        &mut self,
        continuation: NativeContinuation,
        pending: GcRef<PendingSignalWatcherOperation>,
        receiver: Value,
        computed: GcRef<ComputedSignal>,
        changed: bool,
    ) -> Result<(), ExecutionError> {
        let previous = continuation.second();
        self.signal_runtime.computing = signal_previous_computing(previous);
        let old_sources = self.pending_signal_computed_old_sources(pending)?;
        let hooks = self.reconcile_computed_sources(receiver, computed, old_sources)?;
        self.finish_computed_coloring(receiver, changed)?;
        self.clear_signal_computed_from_watcher_pending(receiver)?;
        self.pending_signal_computed_pull_pop(pending)?;
        self.clear_pending_signal_computed_callback_state(pending)?;
        if hooks.is_empty() {
            return self.resume_signal_computed_pull(continuation.site(), pending);
        }
        self.pending_signal_watcher_append_hooks(pending, hooks)?;
        self.resume_signal_watcher_operation(continuation.site(), pending)
    }

    /// Caches a thrown callback completion by identity and resumes parent pull/restoration.
    pub(crate) fn continue_signal_computed_abrupt(
        &mut self,
        continuation: NativeContinuation,
        error: Value,
    ) -> Result<Option<Value>, ExecutionError> {
        let pending = self.pending_signal_watcher_reference(continuation.first())?;
        self.write(
            continuation.site().caller_base,
            continuation.site().destination,
            Value::from_heap_ref(pending.raw()),
        )?;
        let receiver = self
            .pending_signal_computed_pull_top(pending)?
            .ok_or(ExecutionError::MissingNativeContinuation)?
            .computed;
        let computed = self.signal_computed_reference(receiver)?;
        self.heap.with_running_scope(|scope| {
            let computed = scope.root(computed).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(computed, self.types.signal_computed)
                    .map_err(ExecutionError::NoGcBorrow)?;
                node.cached = error;
                node.state = ComputedState::Clean;
                node.generation = (self.signal_runtime.generation & COMPUTED_GENERATION_MASK)
                    | COMPUTED_THROW_COMPLETION_BIT;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(computed, error)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })?;
        let previous = continuation.second();
        self.signal_runtime.computing =
            (previous.as_immediate() != Some(Immediate::Undefined)).then_some(previous);
        let old_sources = self.pending_signal_computed_old_sources(pending)?;
        let hooks = self.reconcile_computed_sources(receiver, computed, old_sources)?;
        self.finish_computed_coloring(receiver, true)?;
        self.clear_signal_computed_from_watcher_pending(receiver)?;
        self.pending_signal_computed_pull_pop(pending)?;
        self.clear_pending_signal_computed_callback_state(pending)?;
        if !hooks.is_empty() {
            self.pending_signal_watcher_append_hooks(pending, hooks)?;
            self.resume_signal_watcher_operation(continuation.site(), pending)?;
            return Ok(None);
        }
        match self.resume_signal_computed_pull(continuation.site(), pending) {
            Ok(()) => Ok(None),
            Err(ExecutionError::HostThrown(error)) => Ok(Some(error)),
            Err(error) => Err(error),
        }
    }

    /// Adds valid signals to a Watcher's ordered set after complete argument validation.
    pub(crate) fn signal_watcher_watch(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.ensure_signal_runtime_unfrozen(site.this_value)?;
        let arguments = self.validated_signal_arguments(site, false)?;
        if site.argument_count == 0 {
            let watcher = self.signal_watcher_reference(site.this_value)?;
            self.heap.with_running_scope(|scope| {
                let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let node = no_gc
                        .borrow_mut(watcher, self.types.signal_watcher)
                        .map_err(ExecutionError::NoGcBorrow)?;
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
            arguments,
        )?;
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
        self.ensure_signal_runtime_unfrozen(site.this_value)?;
        let arguments = self.validated_signal_arguments(site, true)?;
        let pending = self.allocate_pending_signal_watcher_operation(
            site.this_value,
            SignalWatcherOperationKind::Unwatch,
            arguments,
        )?;
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
        arguments: Vec<Value>,
    ) -> Result<GcRef<PendingSignalWatcherOperation>, ExecutionError> {
        let mut hooks = Vec::new();
        hooks
            .try_reserve_exact(tuning::signals::INITIAL_OPERATION_CAPACITY)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        let mut roots = SignalWatcherAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            watcher,
            arguments,
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
        let watcher = roots.watcher;
        let arguments = core::mem::take(&mut roots.arguments);
        drop(roots);
        self.set_pending_signal_watcher_watcher(pending, watcher)?;
        self.set_pending_signal_watcher_arguments(pending, arguments)?;
        Ok(pending)
    }

    /// Allocates the transient, traced DFS stack used by one public Computed.get operation.
    fn allocate_pending_signal_computed_pull(
        &mut self,
        computed: Value,
    ) -> Result<GcRef<PendingSignalWatcherOperation>, ExecutionError> {
        let pending = self.allocate_pending_signal_watcher_operation(
            computed,
            SignalWatcherOperationKind::ComputedPull,
            Vec::new(),
        )?;
        self.pending_signal_computed_pull_push(pending, computed)?;
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
                if node.kind == SignalWatcherOperationKind::Notify {
                    node.hook_index = node.arguments.len();
                }
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
            if snapshot.kind == SignalWatcherOperationKind::Notify {
                if let Some(watcher) = snapshot.argument {
                    self.pending_signal_watcher_advance_argument(pending)?;
                    let callback = self.signal_watcher_notify_value(watcher)?;
                    self.dispatch_frozen_signal_callback(
                        NativeContinuation::signal_watcher_hook(
                            site,
                            Value::from_heap_ref(pending.raw()),
                            watcher,
                        ),
                        callback,
                    )?;
                    return Ok(());
                }
                return self.finish_signal_watcher_notifications(site, pending);
            }
            if let Some(hook) = snapshot.hook {
                self.pending_signal_watcher_advance_hook(pending)?;
                let callback = self.signal_hook_value(hook)?;
                if is_nullish(callback) {
                    continue;
                }
                self.dispatch_frozen_signal_callback(
                    NativeContinuation::signal_watcher_hook(
                        site,
                        Value::from_heap_ref(pending.raw()),
                        hook.signal,
                    ),
                    callback,
                )?;
                return Ok(());
            }
            if snapshot.kind == SignalWatcherOperationKind::ComputedPull {
                self.restore_pending_signal_computed_pull_stack(pending)?;
                return self.resume_signal_computed_pull(site, pending);
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
                    SignalWatcherOperationKind::ComputedPull
                    | SignalWatcherOperationKind::ComputedEquals
                    | SignalWatcherOperationKind::Notify => {
                        return Err(ExecutionError::MissingNativeContinuation);
                    }
                }
                self.pending_signal_watcher_append_hooks(pending, hooks)?;
                continue;
            }
            self.finish_signal_watcher_operation(site, snapshot.watcher, snapshot.kind)?;
            return Ok(());
        }
    }

    /// Freezes graph access until the callback continuation settles or dispatch itself fails.
    fn dispatch_frozen_signal_callback(
        &mut self,
        continuation: NativeContinuation,
        callback: Value,
    ) -> Result<(), ExecutionError> {
        self.signal_runtime.frozen = true;
        match self.dispatch_property_callback(continuation, callback) {
            Ok(_) => Ok(()),
            Err(error) => {
                self.signal_runtime.frozen = false;
                Err(error)
            }
        }
    }

    /// Continues a pending Watcher operation after one lifecycle callback returns.
    pub(crate) fn resume_signal_watcher_hook(
        &mut self,
        continuation: NativeContinuation,
    ) -> Result<(), ExecutionError> {
        let pending = self.pending_signal_watcher_reference(continuation.first())?;
        self.signal_runtime.frozen = false;
        if self.pending_signal_watcher_kind(pending)? == SignalWatcherOperationKind::Notify {
            self.set_signal_watcher_waiting(continuation.second())?;
        }
        self.resume_signal_watcher_operation(continuation.site(), pending)
    }

    /// Saves one notify exception and continues dispatch after abrupt frame unwinding.
    pub(crate) fn continue_signal_watcher_hook_abrupt(
        &mut self,
        continuation: NativeContinuation,
        error: Value,
    ) -> Result<Option<Value>, ExecutionError> {
        let pending = self.pending_signal_watcher_reference(continuation.first())?;
        self.signal_runtime.frozen = false;
        if self.pending_signal_watcher_kind(pending)? != SignalWatcherOperationKind::Notify {
            return Ok(Some(error));
        }
        self.set_signal_watcher_waiting(continuation.second())?;
        self.append_signal_watcher_notify_error(pending, error)?;
        if self
            .pending_signal_watcher_snapshot(pending)?
            .argument
            .is_some()
        {
            self.resume_signal_watcher_operation(continuation.site(), pending)?;
            return Ok(None);
        }
        self.signal_watcher_notification_error(continuation.site(), pending)
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
                    argument: (node.kind != SignalWatcherOperationKind::Notify
                        || node.argument_index < node.hook_index)
                        .then(|| node.arguments.get(node.argument_index).copied())
                        .flatten(),
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

    fn pending_signal_watcher_kind(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<SignalWatcherOperationKind, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(pending, self.types.pending_signal_watcher_operation)
                    .map(|node| node.kind)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn pending_signal_watcher_subject(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(pending, self.types.pending_signal_watcher_operation)
                    .map(|node| node.watcher)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn pending_signal_computed_pull_top(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<Option<SignalComputedPullFrame>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(pending, self.types.pending_signal_watcher_operation)
                    .map(|node| {
                        node.hooks.last().and_then(|frame| {
                            (frame.kind == SignalLifecycleHookKind::Pull).then_some(
                                SignalComputedPullFrame {
                                    computed: frame.signal,
                                    next_source: frame.next_source as usize,
                                },
                            )
                        })
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Pushes a rooted DFS frame and publishes its Value edge before any later GC point.
    fn pending_signal_computed_pull_push(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
        computed: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(pending, self.types.pending_signal_watcher_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                node.hooks
                    .try_reserve(1)
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                node.hooks.push(SignalLifecycleHook {
                    signal: computed,
                    kind: SignalLifecycleHookKind::Pull,
                    next_source: 0,
                });
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(pending, computed)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    fn pending_signal_computed_pull_pop(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(pending, self.types.pending_signal_watcher_operation)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .hooks
                    .pop()
                    .filter(|frame| frame.kind == SignalLifecycleHookKind::Pull)
                    .ok_or(ExecutionError::MissingNativeContinuation)?;
                Ok(())
            })
        })
    }

    fn pending_signal_computed_pull_advance(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(pending, self.types.pending_signal_watcher_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let frame = node
                    .hooks
                    .last_mut()
                    .ok_or(ExecutionError::MissingNativeContinuation)?;
                if frame.kind != SignalLifecycleHookKind::Pull {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
                frame.next_source = frame
                    .next_source
                    .checked_add(1)
                    .ok_or(ExecutionError::PropertyStorageAllocationFailed)?;
                Ok(())
            })
        })
    }

    /// Clears callback-local roots while retaining the bounded pull stack itself.
    fn clear_pending_signal_computed_callback_state(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(pending, self.types.pending_signal_watcher_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                node.arguments.clear();
                node.argument_index = node.hooks.len();
                node.hook_index = node.hooks.len();
                node.kind = SignalWatcherOperationKind::ComputedPull;
                Ok(())
            })
        })
    }

    /// Drops the drained lifecycle suffix and reveals the traced DFS prefix again.
    fn restore_pending_signal_computed_pull_stack(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(pending, self.types.pending_signal_watcher_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                node.hooks.truncate(node.argument_index);
                node.hook_index = node.hooks.len();
                Ok(())
            })
        })
    }

    /// Publishes the comparator argument state after the old-source prefix.
    fn prepare_pending_signal_computed_equals(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
        arguments: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let arguments = Value::from_heap_ref(arguments.raw());
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(pending, self.types.pending_signal_watcher_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                node.argument_index = node.arguments.len();
                node.arguments
                    .try_reserve(1)
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                node.arguments.push(arguments);
                node.kind = SignalWatcherOperationKind::ComputedEquals;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(pending, arguments)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    fn pending_signal_computed_equals_arguments(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<(Value, Value), ExecutionError> {
        let state = self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow(pending, self.types.pending_signal_watcher_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if node.kind != SignalWatcherOperationKind::ComputedEquals {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
                node.arguments
                    .get(node.argument_index)
                    .copied()
                    .ok_or(ExecutionError::MissingNativeContinuation)
            })
        })?;
        let state = self.native_call_state_reference(state)?;
        let snapshot = self.native_call_state_snapshot(state)?;
        Ok((snapshot.values[0], snapshot.values[1]))
    }

    /// Copies only the old-source prefix, leaving comparator arguments rooted in the operation.
    fn pending_signal_computed_old_sources(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<Vec<Value>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow(pending, self.types.pending_signal_watcher_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let end = if node.kind == SignalWatcherOperationKind::ComputedEquals {
                    node.argument_index
                } else {
                    node.arguments.len()
                };
                let mut sources = Vec::new();
                sources
                    .try_reserve_exact(end)
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                sources.extend_from_slice(&node.arguments[..end]);
                Ok(sources)
            })
        })
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
        hooks: Vec<SignalLifecycleHook>,
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
                    .write_value_barrier(pending, hook.signal)
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(())
        })
    }

    /// Returns the lifecycle hook attached to either native Signal node kind.
    fn signal_hook_value(&mut self, hook: SignalLifecycleHook) -> Result<Value, ExecutionError> {
        if hook.kind == SignalLifecycleHookKind::Pull {
            return Err(ExecutionError::MissingNativeContinuation);
        }
        if let Ok(state) = self.signal_state_reference(hook.signal) {
            return self.heap.with_running_scope(|scope| {
                let state = scope.root(state).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let node = no_gc
                        .borrow(state, self.types.signal_state)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    Ok(match hook.kind {
                        SignalLifecycleHookKind::Watched => node.watched,
                        SignalLifecycleHookKind::Unwatched => node.unwatched,
                        SignalLifecycleHookKind::Pull => {
                            unreachable!("pull frames are not callbacks")
                        }
                    })
                })
            });
        }
        let computed = self.signal_computed_reference(hook.signal)?;
        let storage = self.computed_snapshot(computed)?.3;
        let callbacks = self.signal_computed_callbacks(storage)?;
        Ok(match hook.kind {
            SignalLifecycleHookKind::Watched => callbacks.watched,
            SignalLifecycleHookKind::Unwatched => callbacks.unwatched,
            SignalLifecycleHookKind::Pull => unreachable!("pull frames are not callbacks"),
        })
    }

    fn signal_watcher_notify_value(&mut self, watcher: Value) -> Result<Value, ExecutionError> {
        let watcher = self.signal_watcher_reference(watcher)?;
        self.heap.with_running_scope(|scope| {
            let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(watcher, self.types.signal_watcher)
                    .map(|node| node.notify)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn set_signal_watcher_waiting(&mut self, watcher: Value) -> Result<(), ExecutionError> {
        let watcher = self.signal_watcher_reference(watcher)?;
        self.heap.with_running_scope(|scope| {
            let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(watcher, self.types.signal_watcher)
                    .map(|node| node.state = WatcherState::Waiting)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn append_signal_watcher_notify_error(
        &mut self,
        pending: GcRef<PendingSignalWatcherOperation>,
        error: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pending = scope.root(pending).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(pending, self.types.pending_signal_watcher_operation)
                    .map_err(ExecutionError::NoGcBorrow)?;
                node.arguments
                    .try_reserve(1)
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                node.arguments.push(error);
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(pending, error)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Applies one argument's graph transition and queues its first/last live hooks.
    fn prepare_signal_watch(
        &mut self,
        signal: Value,
        watcher: Value,
        hooks: &mut Vec<SignalLifecycleHook>,
    ) -> Result<(), ExecutionError> {
        let watcher_ref = self.signal_watcher_reference(watcher)?;
        let initially_pending = self.signal_computed_needs_pull(signal)?;
        let inserted = self.heap.with_running_scope(|scope| {
            let watcher = scope.root(watcher_ref).map_err(ExecutionError::Root)?;
            let result = scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(watcher, self.types.signal_watcher)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if node.watched.entries.contains(&signal) {
                    return Ok((false, false));
                }
                if node.watched.entries.len() == node.watched.entries.capacity() {
                    node.watched
                        .entries
                        .try_reserve_exact(1)
                        .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                }
                let pending_inserted = initially_pending && !node.pending.entries.contains(&signal);
                if pending_inserted && node.pending.entries.len() == node.pending.entries.capacity()
                {
                    node.pending
                        .entries
                        .try_reserve_exact(1)
                        .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                }
                node.watched.entries.push(signal);
                if pending_inserted {
                    node.pending.entries.push(signal);
                }
                node.state = WatcherState::Watching;
                Ok::<_, ExecutionError>((true, pending_inserted))
            })?;
            if result.0 || result.1 {
                scope
                    .write_value_barrier(watcher, signal)
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(result.0)
        })?;
        if inserted {
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
        hooks: &mut Vec<SignalLifecycleHook>,
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
        kind: SignalWatcherOperationKind,
    ) -> Result<(), ExecutionError> {
        let watcher = self.signal_watcher_reference(watcher)?;
        self.heap.with_running_scope(|scope| {
            let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(watcher, self.types.signal_watcher)
                    .map(|node| {
                        if kind == SignalWatcherOperationKind::Watch
                            && !node.watched.entries.is_empty()
                        {
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
        self.ensure_signal_runtime_unfrozen(site.this_value)?;
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

    /// Resolves proposal option keys from the constructor's defining Realm.
    fn signal_option_symbols(
        &mut self,
        constructor: Value,
    ) -> Result<(Value, Value), ExecutionError> {
        let realm_id = self.realm_for_callable(constructor)?;
        let realm = if realm_id == self.active_realm {
            &self.realm
        } else {
            self.inactive_realms
                .iter()
                .find(|(id, _)| *id == realm_id)
                .map(|(_, realm)| realm)
                .ok_or(ExecutionError::MissingNativeContinuation)?
        };
        Ok((
            realm
                .signal_watched_symbol
                .ok_or(ExecutionError::MissingNativeContinuation)?,
            realm
                .signal_unwatched_symbol
                .ok_or(ExecutionError::MissingNativeContinuation)?,
        ))
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
    ) -> Result<(ComputedState, Vec<Value>, Value, Value, u64), ExecutionError> {
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
                            node.generation,
                        ))
                    })
            })
        })
    }

    /// Resolves the direct callback or the cold custom-equals sidecar representation.
    fn signal_computed_callbacks(
        &mut self,
        storage: Value,
    ) -> Result<SignalComputedCallbacks, ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let Some(raw) = storage.as_heap_ref() else {
            return Ok(SignalComputedCallbacks {
                callback: storage,
                equals: undefined,
                watched: undefined,
                unwatched: undefined,
            });
        };
        let Ok(sidecar) = self
            .heap
            .checked_reference(raw, self.types.native_call_state)
        else {
            return Ok(SignalComputedCallbacks {
                callback: storage,
                equals: undefined,
                watched: undefined,
                unwatched: undefined,
            });
        };
        let snapshot = self.native_call_state_snapshot(sidecar)?;
        Ok(SignalComputedCallbacks {
            callback: snapshot.values[COMPUTED_CALLBACK_SLOT],
            equals: snapshot.values[COMPUTED_EQUALS_SLOT],
            watched: snapshot.values[COMPUTED_WATCHED_SLOT],
            unwatched: snapshot.values[COMPUTED_UNWATCHED_SLOT],
        })
    }

    /// Reads one ordered source without retaining an untraced Value across a GC point.
    fn computed_pull_snapshot(
        &mut self,
        computed: GcRef<ComputedSignal>,
        source_index: usize,
    ) -> Result<(ComputedState, Option<Value>), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let computed = scope.root(computed).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow(computed, self.types.signal_computed)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok((node.state, node.sources.entries.get(source_index).copied()))
            })
        })
    }

    /// Returns or rethrows the requested root only after every pull frame has settled.
    fn finish_signal_computed_pull(
        &mut self,
        site: NativeContinuationSite,
        pending: GcRef<PendingSignalWatcherOperation>,
    ) -> Result<(), ExecutionError> {
        let receiver = self.pending_signal_watcher_subject(pending)?;
        let computed = self.signal_computed_reference(receiver)?;
        let snapshot = self.computed_snapshot(computed)?;
        if snapshot.0 != ComputedState::Clean {
            return Err(ExecutionError::MissingNativeContinuation);
        }
        self.record_signal_dependency(receiver)?;
        if computed_generation_is_throw(snapshot.4) {
            return Err(ExecutionError::HostThrown(snapshot.2));
        }
        self.write(site.caller_base, site.destination, snapshot.2)
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

    /// Clears the current source buffer after its traced old snapshot is published.
    fn clear_computed_sources(
        &mut self,
        computed: GcRef<ComputedSignal>,
    ) -> Result<(), ExecutionError> {
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

    /// Diffs old/new ordered sources and applies only changed reverse and live edges.
    fn reconcile_computed_sources(
        &mut self,
        receiver: Value,
        computed: GcRef<ComputedSignal>,
        old_sources: Vec<Value>,
    ) -> Result<Vec<SignalLifecycleHook>, ExecutionError> {
        let (new_sources, live) = self.heap.with_running_scope(|scope| {
            let computed = scope.root(computed).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow(computed, self.types.signal_computed)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok((node.sources.try_snapshot()?, node.live_sinks != 0))
            })
        })?;
        let mut hooks = Vec::new();
        hooks
            .try_reserve(tuning::signals::INITIAL_OPERATION_CAPACITY)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        for source in old_sources.iter().copied() {
            if new_sources.contains(&source) {
                continue;
            }
            if live {
                self.detach_signal_liveness(source, &mut hooks)?;
            }
            self.remove_signal_sink(source, receiver)?;
        }
        for source in new_sources.iter().copied() {
            if old_sources.contains(&source) {
                continue;
            }
            self.add_signal_sink(source, receiver)?;
            if live {
                self.attach_signal_liveness(source, &mut hooks)?;
            }
        }
        Ok(hooks)
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
            self.heap.with_running_scope(|scope| {
                let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                scope
                    .write_value_barrier(computed, source)
                    .map_err(ExecutionError::HeapReference)
                    .map(|_| ())
            })?;
        }
        Ok(())
    }

    /// Adds one recursively live sink and queues first-live State hooks in source order.
    fn attach_signal_liveness(
        &mut self,
        source: Value,
        hooks: &mut Vec<SignalLifecycleHook>,
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
                    hooks.push(SignalLifecycleHook {
                        signal: current,
                        kind: SignalLifecycleHookKind::Watched,
                        next_source: 0,
                    });
                }
                continue;
            }
            let computed = self.signal_computed_reference(current)?;
            let (first, sources, storage) = self.heap.with_running_scope(|scope| {
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
                    Ok((first, node.sources.try_snapshot()?, node.callback))
                })
            })?;
            if first {
                let watched = self.signal_computed_callbacks(storage)?.watched;
                if !is_nullish(watched) {
                    hooks
                        .try_reserve(1)
                        .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                    hooks.push(SignalLifecycleHook {
                        signal: current,
                        kind: SignalLifecycleHookKind::Watched,
                        next_source: 0,
                    });
                }
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
        hooks: &mut Vec<SignalLifecycleHook>,
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
                    hooks.push(SignalLifecycleHook {
                        signal: current,
                        kind: SignalLifecycleHookKind::Unwatched,
                        next_source: 0,
                    });
                }
                continue;
            }
            let computed = self.signal_computed_reference(current)?;
            let (last, sources, storage) = self.heap.with_running_scope(|scope| {
                let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let node = no_gc
                        .borrow_mut(computed, self.types.signal_computed)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    if node.live_sinks == 0 {
                        return Err(ExecutionError::PropertyStorageAllocationFailed);
                    }
                    node.live_sinks -= 1;
                    Ok((
                        node.live_sinks == 0,
                        node.sources.try_snapshot()?,
                        node.callback,
                    ))
                })
            })?;
            if last {
                let unwatched = self.signal_computed_callbacks(storage)?.unwatched;
                if !is_nullish(unwatched) {
                    hooks
                        .try_reserve(1)
                        .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                    hooks.push(SignalLifecycleHook {
                        signal: current,
                        kind: SignalLifecycleHookKind::Unwatched,
                        next_source: 0,
                    });
                }
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

    /// Colors direct dependents Dirty and transitive dependents Checked in ordered DFS.
    fn propagate_signal_change(&mut self, sinks: Vec<Value>) -> Result<Vec<Value>, ExecutionError> {
        self.signal_runtime.worklist.clear();
        self.signal_runtime
            .worklist
            .try_reserve(sinks.len().max(tuning::signals::INITIAL_WORKLIST_CAPACITY))
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        let mut watchers = Vec::new();
        watchers
            .try_reserve(tuning::signals::INITIAL_OPERATION_CAPACITY)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        for sink in sinks {
            if let Ok(computed) = self.signal_computed_reference(sink) {
                let downstream = self.heap.with_running_scope(|scope| {
                    let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        let node = no_gc
                            .borrow_mut(computed, self.types.signal_computed)
                            .map_err(ExecutionError::NoGcBorrow)?;
                        if node.state == ComputedState::Clean {
                            node.state = ComputedState::Dirty;
                        }
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
            } else if self.mark_signal_watcher_pending(sink)? {
                watchers
                    .try_reserve(1)
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                watchers.push(sink);
            }
        }
        while let Some(sink) = self.signal_runtime.worklist.pop() {
            if let Ok(computed) = self.signal_computed_reference(sink) {
                let downstream = self.heap.with_running_scope(|scope| {
                    let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        let node = no_gc
                            .borrow_mut(computed, self.types.signal_computed)
                            .map_err(ExecutionError::NoGcBorrow)?;
                        if node.state != ComputedState::Clean {
                            return Ok(Vec::new());
                        }
                        node.state = ComputedState::Checked;
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
            } else if self.mark_signal_watcher_pending(sink)? {
                watchers
                    .try_reserve(1)
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                watchers.push(sink);
            }
        }
        Ok(watchers)
    }

    /// Refreshes dirty Computeds and moves only an armed Watcher into Pending.
    fn mark_signal_watcher_pending(&mut self, watcher: Value) -> Result<bool, ExecutionError> {
        let watcher = self.signal_watcher_reference(watcher)?;
        let (state, watched) = self.heap.with_running_scope(|scope| {
            let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(watcher, self.types.signal_watcher)
                    .map_err(ExecutionError::NoGcBorrow)
                    .and_then(|node| Ok((node.state, node.watched.try_snapshot()?)))
            })
        })?;
        let mut pending = Vec::new();
        pending
            .try_reserve_exact(watched.len())
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        for signal in watched {
            if self.signal_computed_needs_pull(signal)? {
                pending.push(signal);
            }
        }
        self.heap.with_running_scope(|scope| {
            let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
            let marked = scope.with_no_gc_scope(|no_gc| {
                let node = no_gc
                    .borrow_mut(watcher, self.types.signal_watcher)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let additional = pending
                    .iter()
                    .filter(|signal| !node.pending.entries.contains(signal))
                    .count();
                if node.pending.entries.capacity() - node.pending.entries.len() < additional {
                    node.pending
                        .entries
                        .try_reserve_exact(additional)
                        .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                }
                for signal in pending.iter().copied() {
                    node.pending.insert(signal)?;
                }
                if state == WatcherState::Watching {
                    node.state = WatcherState::Pending;
                }
                Ok(state == WatcherState::Watching)
            })?;
            for signal in pending {
                scope
                    .write_value_barrier(watcher, signal)
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(marked)
        })
    }

    /// Reports whether a value is a Computed whose cached completion needs validation.
    fn signal_computed_needs_pull(&mut self, signal: Value) -> Result<bool, ExecutionError> {
        let Ok(computed) = self.signal_computed_reference(signal) else {
            return Ok(false);
        };
        self.heap.with_running_scope(|scope| {
            let computed = scope.root(computed).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(computed, self.types.signal_computed)
                    .map(|node| node.state != ComputedState::Clean)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Removes one settled Computed from every directly attached Watcher's pending subset.
    fn clear_signal_computed_from_watcher_pending(
        &mut self,
        signal: Value,
    ) -> Result<(), ExecutionError> {
        let computed = self.signal_computed_reference(signal)?;
        let sinks = self.heap.with_running_scope(|scope| {
            let computed = scope.root(computed).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(computed, self.types.signal_computed)
                    .map_err(ExecutionError::NoGcBorrow)
                    .and_then(|node| node.sinks.try_snapshot())
            })
        })?;
        for sink in sinks {
            let Ok(watcher) = self.signal_watcher_reference(sink) else {
                continue;
            };
            self.heap.with_running_scope(|scope| {
                let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(watcher, self.types.signal_watcher)
                        .map(|node| {
                            node.pending.remove(signal);
                        })
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
        }
        Ok(())
    }

    /// Promotes checked downstream nodes on change or cleans unchanged checked chains.
    fn finish_computed_coloring(
        &mut self,
        computed: Value,
        changed: bool,
    ) -> Result<(), ExecutionError> {
        let computed = self.signal_computed_reference(computed)?;
        let sinks = self.heap.with_running_scope(|scope| {
            let computed = scope.root(computed).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(computed, self.types.signal_computed)
                    .map_err(ExecutionError::NoGcBorrow)
                    .and_then(|node| node.sinks.try_snapshot())
            })
        })?;
        self.signal_runtime.worklist.clear();
        self.signal_runtime
            .worklist
            .try_reserve(sinks.len().max(tuning::signals::INITIAL_WORKLIST_CAPACITY))
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        self.signal_runtime.worklist.extend(sinks.into_iter().rev());
        while let Some(sink) = self.signal_runtime.worklist.pop() {
            let Ok(computed) = self.signal_computed_reference(sink) else {
                continue;
            };
            let (state, sources) = self.heap.with_running_scope(|scope| {
                let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let node = no_gc
                        .borrow(computed, self.types.signal_computed)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    Ok((node.state, node.sources.try_snapshot()?))
                })
            })?;
            if state != ComputedState::Checked
                || (!changed && !self.signal_sources_are_clean(&sources)?)
            {
                continue;
            }
            let downstream = self.heap.with_running_scope(|scope| {
                let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let node = no_gc
                        .borrow_mut(computed, self.types.signal_computed)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    if node.state != ComputedState::Checked {
                        return Ok(Vec::new());
                    }
                    node.state = if changed {
                        ComputedState::Dirty
                    } else {
                        ComputedState::Clean
                    };
                    node.sinks.try_snapshot()
                })
            })?;
            if !changed {
                self.clear_signal_computed_from_watcher_pending(sink)?;
            }
            self.signal_runtime
                .worklist
                .try_reserve(downstream.len())
                .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
            self.signal_runtime
                .worklist
                .extend(downstream.into_iter().rev());
        }
        Ok(())
    }

    fn signal_sources_are_clean(&mut self, sources: &[Value]) -> Result<bool, ExecutionError> {
        for source in sources.iter().copied() {
            let Ok(computed) = self.signal_computed_reference(source) else {
                continue;
            };
            let clean = self.heap.with_running_scope(|scope| {
                let computed = scope.root(computed).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(computed, self.types.signal_computed)
                        .map(|node| node.state == ComputedState::Clean)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            if !clean {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[inline(always)]
    fn ensure_signal_runtime_unfrozen(&self, receiver: Value) -> Result<(), ExecutionError> {
        if self.signal_runtime.frozen {
            return Err(ExecutionError::NotObject(receiver));
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
