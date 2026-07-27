use super::*;

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
    pub(super) fn allocate_signal_state_call_state(
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
    pub(super) fn finish_signal_watcher_notifications(
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
    pub(super) fn signal_watcher_notification_error(
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

    /// Reads one rooted notify error from the pending operation's ordered tail segment.
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
}
