use super::*;

impl Isolate {
    /// Restores all agent-wide Signal state before a suspended execution is discarded.
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

    #[inline(always)]
    pub(crate) fn signal_is_state(&mut self, value: Value) -> Value {
        Self::signal_brand_result(self.signal_state_reference(value).is_ok())
    }

    #[inline(always)]
    pub(crate) fn signal_is_computed(&mut self, value: Value) -> Value {
        Self::signal_brand_result(self.signal_computed_reference(value).is_ok())
    }

    #[inline(always)]
    pub(crate) fn signal_is_watcher(&mut self, value: Value) -> Value {
        Self::signal_brand_result(self.signal_watcher_reference(value).is_ok())
    }

    #[inline(always)]
    fn signal_brand_result(matches: bool) -> Value {
        Value::from_immediate(if matches {
            Immediate::True
        } else {
            Immediate::False
        })
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
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                inactive_realms: &mut self.inactive_realms,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
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
}
