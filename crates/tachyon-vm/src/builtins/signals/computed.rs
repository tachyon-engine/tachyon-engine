use super::*;

impl Isolate {
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
    pub(super) fn resume_signal_computed_pull(
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
    pub(super) fn restore_failed_signal_computed_start(
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
}
