use super::*;

impl Isolate {
    pub(super) fn signal_state_reference(
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

    pub(super) fn signal_computed_reference(
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

    pub(super) fn signal_watcher_reference(
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

    /// Copies the complete observable Computed state while its GC reference is rooted.
    pub(super) fn computed_snapshot(
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
    pub(super) fn signal_computed_callbacks(
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
    pub(super) fn computed_pull_snapshot(
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
    pub(super) fn finish_signal_computed_pull(
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

    pub(super) fn set_computed_state(
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
    pub(super) fn clear_computed_sources(
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
    pub(super) fn reconcile_computed_sources(
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
            self.add_signal_sink(source, receiver, live)?;
            if live {
                self.attach_signal_liveness(source, &mut hooks)?;
            }
        }
        Ok(hooks)
    }

    /// Adds a dependency in first-read order and publishes the reverse edge with a barrier.
    pub(super) fn record_signal_dependency(&mut self, source: Value) -> Result<(), ExecutionError> {
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
    pub(super) fn attach_signal_liveness(
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
                for source in sources.iter().copied() {
                    self.set_signal_sink_live(source, current, true)?;
                }
                work.try_reserve(sources.len())
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                work.extend(sources.into_iter().rev());
            }
        }
        Ok(())
    }

    /// Removes one recursively live sink and queues last-live State hooks.
    pub(super) fn detach_signal_liveness(
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
                for source in sources.iter().copied() {
                    self.set_signal_sink_live(source, current, false)?;
                }
                work.try_reserve(sources.len())
                    .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
                work.extend(sources.into_iter().rev());
            }
        }
        Ok(())
    }

    /// Inserts one reverse dependency edge and publishes its generational barrier.
    pub(super) fn add_signal_sink(
        &mut self,
        source: Value,
        sink: Value,
        live: bool,
    ) -> Result<(), ExecutionError> {
        if let Ok(state) = self.signal_state_reference(source) {
            self.heap.with_running_scope(|scope| {
                let state = scope.root(state).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(state, self.types.signal_state)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .sinks
                        .insert(sink, live)
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
                        .insert(sink, live)
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

    /// Promotes or demotes one reverse edge as its dependent enters or leaves the live graph.
    fn set_signal_sink_live(
        &mut self,
        source: Value,
        sink: Value,
        live: bool,
    ) -> Result<(), ExecutionError> {
        if let Ok(state) = self.signal_state_reference(source) {
            self.heap.with_running_scope(|scope| {
                let state = scope.root(state).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(state, self.types.signal_state)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .sinks
                        .set_live(sink, live)
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
                        .set_live(sink, live)
                })
            })?;
        }
        if live && let (Some(source), Some(sink)) = (source.as_heap_ref(), sink.as_heap_ref()) {
            self.heap
                .write_barrier(source, sink)
                .map_err(|_| ExecutionError::NotObject(Value::from_heap_ref(source)))?;
        }
        Ok(())
    }

    /// Removes one reverse dependency edge from either supported source node kind.
    pub(super) fn remove_signal_sink(
        &mut self,
        source: Value,
        sink: Value,
    ) -> Result<(), ExecutionError> {
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
    pub(super) fn propagate_signal_change(
        &mut self,
        sinks: Vec<Value>,
    ) -> Result<Vec<Value>, ExecutionError> {
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
    pub(super) fn signal_computed_needs_pull(
        &mut self,
        signal: Value,
    ) -> Result<bool, ExecutionError> {
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
    pub(super) fn clear_signal_computed_from_watcher_pending(
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
    pub(super) fn finish_computed_coloring(
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

    /// Checks whether every Computed source has completed checked-pull cleanup.
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
    pub(super) fn ensure_signal_runtime_unfrozen(
        &self,
        receiver: Value,
    ) -> Result<(), ExecutionError> {
        if self.signal_runtime.frozen {
            return Err(ExecutionError::NotObject(receiver));
        }
        Ok(())
    }

    /// Validates the whole watch/unwatch list before any graph mutation occurs.
    pub(super) fn validated_signal_arguments(
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
