use super::*;

impl Isolate {
    /// Adds valid signals to a Watcher's ordered set after complete argument validation.
    pub(crate) fn signal_watcher_watch(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.ensure_signal_runtime_unfrozen(site.this_value)?;
        let arguments = self.validated_signal_arguments(site)?;
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

    /// Removes valid signals from a Watcher without partial mutation on argument errors.
    pub(crate) fn signal_watcher_unwatch(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.ensure_signal_runtime_unfrozen(site.this_value)?;
        let arguments = self.validated_signal_arguments(site)?;
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
    pub(super) fn allocate_pending_signal_watcher_operation(
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
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
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
    pub(super) fn allocate_pending_signal_computed_pull(
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

    /// Replaces the rooted operation subject and publishes the new GC edge.
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
    pub(super) fn set_pending_signal_watcher_arguments(
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
    pub(super) fn resume_signal_watcher_operation(
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

    pub(super) fn pending_signal_watcher_reference(
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

    pub(super) fn pending_signal_watcher_kind(
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

    pub(super) fn pending_signal_watcher_subject(
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

    /// Decodes the top iterative Computed pull frame from traced hook storage.
    pub(super) fn pending_signal_computed_pull_top(
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
    pub(super) fn pending_signal_computed_pull_push(
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

    pub(super) fn pending_signal_computed_pull_pop(
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

    /// Advances the top pull frame after its current source has been classified.
    pub(super) fn pending_signal_computed_pull_advance(
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
    pub(super) fn clear_pending_signal_computed_callback_state(
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
    pub(super) fn prepare_pending_signal_computed_equals(
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

    /// Reads the rooted old/new pair for a suspended custom equality callback.
    pub(super) fn pending_signal_computed_equals_arguments(
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
    pub(super) fn pending_signal_computed_old_sources(
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

    /// Appends lifecycle work to traced storage and publishes every signal edge.
    pub(super) fn pending_signal_watcher_append_hooks(
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

    pub(super) fn set_signal_watcher_waiting(
        &mut self,
        watcher: Value,
    ) -> Result<(), ExecutionError> {
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

    /// Appends one notify failure to traced storage before later allocation points.
    pub(super) fn append_signal_watcher_notify_error(
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
            self.add_signal_sink(signal, watcher, true)?;
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

    /// Commits Watcher state after all validated watch or unwatch hooks have run.
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

    /// Resolves a derived constructor prototype with the intrinsic Realm fallback.
    pub(super) fn signal_prototype_for_new_target(
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
    pub(super) fn signal_option_symbols(
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
}
