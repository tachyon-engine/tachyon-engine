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
        self.entries
            .try_reserve(1)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
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
    sinks: OrderedSignals,
    pub(crate) ordinary: OrdinaryObject,
}

impl Trace for StateSignal {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.value.trace(tracer);
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
    generation: u64,
    pub(crate) ordinary: OrdinaryObject,
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
    /// Allocates one State node with the constructor-selected prototype.
    pub(crate) fn create_signal_state_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let value = self
            .call_argument(site, 0)?
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
        self.heap
            .try_allocate_with_gc(
                self.types.signal_state,
                0,
                0,
                StateSignal {
                    value,
                    sinks: OrderedSignals::try_new()?,
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

    /// Applies SameValue and iteratively dirties Computed sinks while queuing Watchers.
    pub(crate) fn signal_state_set(
        &mut self,
        receiver: Value,
        value: Value,
    ) -> Result<Value, ExecutionError> {
        let state = self.signal_state_reference(receiver)?;
        let old = self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.signal_state)
                    .map(|node| node.value)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        if self.same_value(old, value)? {
            return Ok(Value::from_immediate(Immediate::Undefined));
        }
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
        self.propagate_signal_change(sinks)?;
        Ok(Value::from_immediate(Immediate::Undefined))
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
    pub(crate) fn signal_watcher_watch(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let watcher = self.signal_watcher_reference(site.this_value)?;
        let arguments = self.validated_signal_arguments(site, false)?;
        for signal in arguments {
            self.add_signal_sink(signal, site.this_value)?;
            self.heap.with_running_scope(|scope| {
                let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let node = no_gc
                        .borrow_mut(watcher, self.types.signal_watcher)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    node.watched.insert(signal)?;
                    node.state = WatcherState::Watching;
                    Ok(())
                })
            })?;
        }
        if site.argument_count == 0 {
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
        }
        Ok(Value::from_immediate(Immediate::Undefined))
    }

    /// Removes validated watched signals from a Watcher without partial mutation on errors.
    pub(crate) fn signal_watcher_unwatch(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let watcher = self.signal_watcher_reference(site.this_value)?;
        let arguments = self.validated_signal_arguments(site, true)?;
        for signal in arguments {
            self.remove_signal_sink(signal, site.this_value)?;
            self.heap.with_running_scope(|scope| {
                let watcher = scope.root(watcher).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let node = no_gc
                        .borrow_mut(watcher, self.types.signal_watcher)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    node.watched.remove(signal);
                    node.pending.remove(signal);
                    if node.watched.entries.is_empty() {
                        node.state = WatcherState::Waiting;
                    }
                    Ok(())
                })
            })?;
        }
        Ok(Value::from_immediate(Immediate::Undefined))
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
        for source in sources {
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
