//! VM-private exclusion lists and ordinary CopyDataProperties fast paths.

use core::mem::size_of;

use super::super::*;

/// Exact-capacity, non-observable PropertyKey storage for object-rest exclusions.
#[derive(Debug)]
pub(crate) struct ExclusionList {
    keys: Vec<PropertyKey>,
    capacity: usize,
}

impl ExclusionList {
    /// Allocates storage from the compiler's exact bound so appending keys never grows the list.
    fn with_capacity(capacity: usize) -> Result<Self, ExecutionError> {
        let mut keys = Vec::new();
        keys.try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::ExclusionListAllocationFailed)?;
        Ok(Self { keys, capacity })
    }

    #[inline(always)]
    fn contains(&self, key: PropertyKey) -> bool {
        self.keys.contains(&key)
    }

    /// Appends one key after validating the compiler-provided exact list length.
    fn push(&mut self, key: PropertyKey) -> Result<(), ExecutionError> {
        if self.keys.len() == self.capacity {
            return Err(ExecutionError::ExclusionListCapacityExceeded);
        }
        self.keys.push(key);
        Ok(())
    }
}

impl Trace for ExclusionList {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.keys.trace(tracer);
    }
}

/// GC-managed CopyDataProperties state retained while an enumerable getter executes JavaScript.
#[derive(Debug)]
pub(crate) struct PendingCopyDataProperties {
    target: Value,
    source: Value,
    exclusions: Value,
    keys: Box<[PropertyKey]>,
    index: usize,
    consumer: CopyDataPropertiesConsumer,
}

#[derive(Clone, Copy, Debug)]
enum CopyDataPropertiesConsumer {
    Bytecode,
    ObjectAssign(Value),
}

enum CopyDataPropertyAction {
    Continue,
    Dispatched(Option<RunOutcome>),
}

/// GC-managed Object.assign source list retained across observable getters.
#[derive(Debug)]
pub(crate) struct PendingObjectAssign {
    target: Value,
    exclusions: Value,
    sources: Box<[Value]>,
    index: usize,
}

impl Trace for PendingCopyDataProperties {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.target.trace(tracer);
        self.source.trace(tracer);
        self.exclusions.trace(tracer);
        self.keys.trace(tracer);
        if let CopyDataPropertiesConsumer::ObjectAssign(state) = &mut self.consumer {
            state.trace(tracer);
        }
    }
}

impl Trace for PendingObjectAssign {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.target.trace(tracer);
        self.exclusions.trace(tracer);
        self.sources.trace(tracer);
    }
}

impl GcExternalMemory for PendingCopyDataProperties {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.keys.len() * size_of::<PropertyKey>()
    }
}

impl GcExternalMemory for PendingObjectAssign {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.sources.len() * size_of::<Value>()
    }
}

impl Isolate {
    /// Starts Object.assign with an exactly-sized rooted source list.
    pub(crate) fn begin_object_assign(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let target = self.object_value_of(target)?;
        let source_count = site.argument_count.saturating_sub(1) as usize;
        let mut sources = Vec::new();
        sources
            .try_reserve_exact(source_count)
            .map_err(|_| ExecutionError::CopyDataPropertiesAllocationFailed)?;
        for index in 1..site.argument_count {
            sources.push(
                self.call_argument(site, index)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined)),
            );
        }
        let exclusions = self.create_exclusion_list(0)?;
        let state = self.allocate_pending_object_assign(PendingObjectAssign {
            target,
            exclusions,
            sources: sources.into_boxed_slice(),
            index: 0,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.advance_object_assign(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            state,
        )
        .map(|_| ())
    }

    /// Allocates a VM-private list that cannot invoke user code or inherit user-visible properties.
    pub(crate) fn create_exclusion_list(&mut self, capacity: u32) -> Result<Value, ExecutionError> {
        let capacity =
            usize::try_from(capacity).map_err(|_| ExecutionError::ExclusionListAllocationFailed)?;
        let list = ExclusionList::with_capacity(capacity)?;
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
            .try_allocate_with_gc(
                self.types.exclusion_list,
                0,
                0,
                list,
                AllocationSpace::Young,
                roots,
            )
            .map(|reference| Value::from_heap_ref(reference.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Records one pre-normalized exclusion key without allowing a compiler bug to resize storage.
    pub(crate) fn exclude_property_key(
        &mut self,
        list: Value,
        key: Value,
    ) -> Result<(), ExecutionError> {
        let key = self.property_key(key)?;
        let raw = list
            .as_heap_ref()
            .ok_or(ExecutionError::InvalidExclusionList(list))?;
        let list = self
            .heap
            .checked_reference(raw, self.types.exclusion_list)
            .map_err(|_| ExecutionError::InvalidExclusionList(list))?;
        self.heap.with_running_scope(|scope| {
            let list = scope.root(list).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(list, self.types.exclusion_list)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .push(key)
            })?;
            if let Some(symbol) = key.symbol() {
                scope
                    .write_value_barrier(list, symbol.value())
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(())
        })
    }

    /// Starts CopyDataProperties and transfers accessor reads into the explicit VM continuation path.
    pub(crate) fn begin_copy_data_properties(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        source: Value,
        exclusions: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.begin_copy_data_properties_for_consumer(
            site,
            target,
            source,
            exclusions,
            CopyDataPropertiesConsumer::Bytecode,
        )
    }

    /// Starts one copy pass and records which native operation consumes its completion.
    fn begin_copy_data_properties_for_consumer(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        source: Value,
        exclusions: Value,
        consumer: CopyDataPropertiesConsumer,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if matches!(
            source.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(source));
        }
        if !self.is_object_value(source) {
            return self.finish_copy_data_properties(site, target, consumer);
        }
        let raw = exclusions
            .as_heap_ref()
            .ok_or(ExecutionError::InvalidExclusionList(exclusions))?;
        let _exclusion_list = self
            .heap
            .checked_reference(raw, self.types.exclusion_list)
            .map_err(|_| ExecutionError::InvalidExclusionList(exclusions))?;
        if self.is_proxy_value(source) {
            let state = self.allocate_pending_copy_data_properties(PendingCopyDataProperties {
                target,
                source,
                exclusions,
                keys: Box::new([]),
                index: 0,
                consumer,
            })?;
            self.write(
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
            )?;
            return self.dispatch_copy_data_properties_own_keys(site, state, source);
        }
        let (_, snapshot) = self.object_snapshot(source)?;
        let keys = self.ordinary_own_property_keys(source, snapshot)?;
        let string_length = if self.is_string_wrapper(source) {
            self.string_value_length(source)?
        } else {
            0
        };
        let mut copied_keys = Vec::new();
        copied_keys
            .try_reserve_exact(keys.len().saturating_add(string_length))
            .map_err(|_| ExecutionError::CopyDataPropertiesAllocationFailed)?;
        for index in 0..string_length {
            copied_keys.push(PropertyKey::Atom(
                self.safe_integer_property_atom(index as u64)?,
            ));
        }
        copied_keys.extend(keys);
        let state = self.allocate_pending_copy_data_properties(PendingCopyDataProperties {
            target,
            source,
            exclusions,
            keys: copied_keys.into_boxed_slice(),
            index: 0,
            consumer,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.advance_copy_data_properties(site, state)
    }

    /// Continues with the next non-nullish source after one copy pass completes.
    fn advance_object_assign(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingObjectAssign>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let pending = self.pending_object_assign(state)?;
            let Some(source) = pending.source else {
                self.write(site.caller_base, site.destination, pending.target)?;
                return Ok(None);
            };
            self.advance_pending_object_assign(state)?;
            if is_nullish(source) {
                continue;
            }
            let source = self.object_value_of(source)?;
            return self.begin_copy_data_properties_for_consumer(
                site,
                pending.target,
                source,
                pending.exclusions,
                CopyDataPropertiesConsumer::ObjectAssign(Value::from_heap_ref(state.raw())),
            );
        }
    }

    /// Routes a completed copy pass back to bytecode or the Object.assign source loop.
    fn finish_copy_data_properties(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        consumer: CopyDataPropertiesConsumer,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match consumer {
            CopyDataPropertiesConsumer::Bytecode => {
                self.write(site.caller_base, site.destination, target)?;
                Ok(None)
            }
            CopyDataPropertiesConsumer::ObjectAssign(state) => {
                let state = self.pending_object_assign_reference(state)?;
                self.advance_object_assign(site, state)
            }
        }
    }

    /// Records an accessor result, then resumes ordered CopyDataProperties scanning from GC state.
    pub(crate) fn resume_copy_data_properties(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCopyDataProperties>,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let pending = self.pending_copy_data_properties(state)?;
        let key = pending
            .key
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.advance_pending_copy_data_properties(state)?;
        match self.write_copy_data_property(site, state, pending, key, value)? {
            CopyDataPropertyAction::Continue => self.advance_copy_data_properties(site, state),
            CopyDataPropertyAction::Dispatched(outcome) => Ok(outcome),
        }
    }

    /// Resumes Proxy ownKeys, descriptor-enumerability, or Get for one source key.
    pub(crate) fn resume_copy_data_properties_stage(
        &mut self,
        continuation: NativeContinuation,
        stage: CopyDataPropertiesStage,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let site = continuation.site();
        let state = self.pending_copy_data_properties_reference(continuation.first())?;
        match stage {
            CopyDataPropertiesStage::OwnKeys => {
                self.resume_copy_data_properties_own_keys(site, state, value)
            }
            CopyDataPropertiesStage::Enumerable => {
                if !self.is_truthy_value(value)? {
                    return self.advance_copy_data_properties(site, state);
                }
                let key_value = continuation.second();
                let key = self.property_key(key_value)?;
                let source = self.pending_copy_data_properties(state)?.source;
                self.dispatch_copy_data_properties_get(site, state, source, key, key_value)
            }
            CopyDataPropertiesStage::Get => {
                let key = self.property_key(continuation.second())?;
                let pending = self.pending_copy_data_properties(state)?;
                match self.write_copy_data_property(site, state, pending, key, value)? {
                    CopyDataPropertyAction::Continue => {
                        self.advance_copy_data_properties(site, state)
                    }
                    CopyDataPropertyAction::Dispatched(outcome) => Ok(outcome),
                }
            }
        }
    }

    /// Converts a materialized Proxy ownKeys array into the copy state's exact key snapshot.
    fn resume_copy_data_properties_own_keys(
        &mut self,
        site: NativeContinuationSite,
        old_state: GcRef<PendingCopyDataProperties>,
        result: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let length_key = PropertyKey::Atom(self.length_atom()?);
        let length = self
            .get_data_property(result, length_key)?
            .and_then(numeric_value)
            .ok_or(ExecutionError::ArrayLengthOverflow)? as usize;
        let mut keys = Vec::new();
        keys.try_reserve_exact(length)
            .map_err(|_| ExecutionError::CopyDataPropertiesAllocationFailed)?;
        for index in 0..length {
            let index_key = PropertyKey::Atom(self.safe_integer_property_atom(index as u64)?);
            let key = self
                .get_data_property(result, index_key)?
                .ok_or(ExecutionError::ProxyInvariantViolation)?;
            keys.push(self.property_key(key)?);
        }
        let pending = self.pending_copy_data_properties(old_state)?;
        let state = self.allocate_pending_copy_data_properties(PendingCopyDataProperties {
            target: pending.target,
            source: pending.source,
            exclusions: pending.exclusions,
            keys: keys.into_boxed_slice(),
            index: 0,
            consumer: pending.consumer,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.advance_copy_data_properties(site, state)
    }

    /// Processes non-accessor keys synchronously and suspends only at the next observable getter.
    fn advance_copy_data_properties(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCopyDataProperties>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let pending = self.pending_copy_data_properties(state)?;
            let Some(key) = pending.key else {
                return self.finish_copy_data_properties(site, pending.target, pending.consumer);
            };
            self.advance_pending_copy_data_properties(state)?;
            if self.exclusion_list_contains_value(pending.exclusions, key)? {
                continue;
            }
            if self.is_proxy_value(pending.source) {
                let key_value = self.property_key_value(key)?;
                return self.dispatch_copy_data_properties_enumerable(
                    site,
                    state,
                    pending.source,
                    key_value,
                );
            }
            let Some(descriptor) = self.complete_own_property_descriptor(pending.source, key)?
            else {
                continue;
            };
            if !descriptor.enumerable().unwrap_or(false) {
                continue;
            }
            match self.resolve_property_read(pending.source, key)? {
                PropertyRead::Missing => continue,
                PropertyRead::Data(value) => {
                    match self.write_copy_data_property(site, state, pending, key, value)? {
                        CopyDataPropertyAction::Continue => {}
                        CopyDataPropertyAction::Dispatched(outcome) => return Ok(outcome),
                    }
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    match self.write_copy_data_property(
                        site,
                        state,
                        pending,
                        key,
                        Value::from_immediate(Immediate::Undefined),
                    )? {
                        CopyDataPropertyAction::Continue => {}
                        CopyDataPropertyAction::Dispatched(outcome) => return Ok(outcome),
                    }
                }
                PropertyRead::Accessor(callee) => {
                    self.rewind_pending_copy_data_properties(state)?;
                    return self.dispatch_property_callback(
                        NativeContinuation::copy_data_properties(
                            site,
                            Value::from_heap_ref(state.raw()),
                        ),
                        callee,
                    );
                }
            }
        }
    }

    fn property_key_value(&mut self, key: PropertyKey) -> Result<Value, ExecutionError> {
        match key {
            PropertyKey::Atom(atom) => self.atom_string_value(atom),
            PropertyKey::Symbol(symbol) => Ok(symbol.value()),
            PropertyKey::Private(_) => Err(ExecutionError::PrivatePropertyKeyEscaped),
        }
    }

    /// Defines the CreateDataProperty result without consulting target prototypes or setters.
    fn copy_data_property(
        &mut self,
        target: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.define_data_property(
            target,
            key,
            DataPropertyDescriptor {
                value: Some(value),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
            },
        )
    }

    /// Applies CreateDataProperty or Set according to the active copy consumer.
    fn write_copy_data_property(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCopyDataProperties>,
        pending: PendingCopyDataPropertiesSnapshot,
        key: PropertyKey,
        value: Value,
    ) -> Result<CopyDataPropertyAction, ExecutionError> {
        if matches!(pending.consumer, CopyDataPropertiesConsumer::Bytecode) {
            self.copy_data_property(pending.target, key, value)?;
            return Ok(CopyDataPropertyAction::Continue);
        }
        match self.resolve_property_write_until_proxy(pending.target, key, value)? {
            PropertyWriteResolution::Write(PropertyWrite::Complete(true)) => {
                Ok(CopyDataPropertyAction::Continue)
            }
            PropertyWriteResolution::Write(PropertyWrite::Complete(false)) => {
                Err(ExecutionError::ReadOnlyProperty(pending.target))
            }
            PropertyWriteResolution::Write(PropertyWrite::Setter(callee)) => self
                .write(site.caller_base, site.destination, value)
                .and_then(|()| {
                    self.dispatch_property_callback(
                        NativeContinuation::object_assign_set(
                            site,
                            Value::from_heap_ref(state.raw()),
                            value,
                        ),
                        callee,
                    )
                })
                .map(CopyDataPropertyAction::Dispatched),
            PropertyWriteResolution::Proxy(proxy) => self.dispatch_object_assign_proxy_write(
                site,
                state,
                proxy,
                pending.target,
                key,
                value,
            ),
        }
    }

    /// Resumes the copy scan after one Object.assign target setter completes.
    pub(crate) fn resume_object_assign_set(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCopyDataProperties>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.advance_copy_data_properties(site, state)
    }

    /// Publishes a parent continuation while Proxy [[Set]] runs for Object.assign.
    fn dispatch_object_assign_proxy_write(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCopyDataProperties>,
        proxy: Value,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<CopyDataPropertyAction, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::object_assign_set(
                site,
                Value::from_heap_ref(state.raw()),
                value,
            ))
            .map_err(Self::completion_stack_error)?;
        let outcome = self.dispatch_proxy_aware_property_write(
            site,
            proxy,
            receiver,
            key,
            value,
            ProxySetMode::ObjectAssign,
        );
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() == completion_depth
        {
            return outcome.map(CopyDataPropertyAction::Dispatched);
        }
        self.pop_native_continuation()?;
        self.resume_object_assign_set(site, state)
            .map(CopyDataPropertyAction::Dispatched)
    }

    /// Requests Proxy [[OwnPropertyKeys]] while retaining the empty copy state as parent.
    fn dispatch_copy_data_properties_own_keys(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCopyDataProperties>,
        source: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let continuation = NativeContinuation::copy_data_properties_stage(
            site,
            CopyDataPropertiesStage::OwnKeys,
            Value::from_heap_ref(state.raw()),
            Value::from_immediate(Immediate::Undefined),
        );
        self.dispatch_copy_data_properties_proxy_operation(continuation, |isolate| {
            isolate.dispatch_proxy_own_keys(site, source, ProxyOwnKeysMode::Internal)
        })
    }

    /// Requests Proxy [[GetOwnProperty]] and resumes only for enumerable keys.
    fn dispatch_copy_data_properties_enumerable(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCopyDataProperties>,
        source: Value,
        key: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let continuation = NativeContinuation::copy_data_properties_stage(
            site,
            CopyDataPropertiesStage::Enumerable,
            Value::from_heap_ref(state.raw()),
            key,
        );
        self.dispatch_copy_data_properties_proxy_operation(continuation, |isolate| {
            isolate.dispatch_proxy_get_own(site, source, key, ProxyGetOwnMode::Enumerable)
        })
    }

    /// Requests Proxy [[Get]] for an enumerable source key.
    fn dispatch_copy_data_properties_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCopyDataProperties>,
        source: Value,
        key: PropertyKey,
        key_value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let continuation = NativeContinuation::copy_data_properties_stage(
            site,
            CopyDataPropertiesStage::Get,
            Value::from_heap_ref(state.raw()),
            key_value,
        );
        self.dispatch_copy_data_properties_proxy_operation(continuation, |isolate| {
            isolate.dispatch_proxy_aware_property_read(site, source, source, key)
        })
    }

    /// Runs one Proxy operation and drains its parent continuation on synchronous completion.
    fn dispatch_copy_data_properties_proxy_operation(
        &mut self,
        continuation: NativeContinuation,
        operation: impl FnOnce(&mut Self) -> Result<Option<RunOutcome>, ExecutionError>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let outcome = operation(self);
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() == completion_depth
        {
            return outcome;
        }
        let continuation = self.pop_native_continuation()?;
        let site = continuation.site();
        let value = self.read(site.caller_base, site.destination)?;
        let NativeContinuationKind::CopyDataProperties(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_copy_data_properties_stage(continuation, stage, value)
    }

    fn exclusion_list_contains(
        &mut self,
        list: GcRef<ExclusionList>,
        key: PropertyKey,
    ) -> Result<bool, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let list = scope.root(list).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(list, self.types.exclusion_list)
                    .map(|list| list.contains(key))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn exclusion_list_contains_value(
        &mut self,
        value: Value,
        key: PropertyKey,
    ) -> Result<bool, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::InvalidExclusionList(value))?;
        let list = self
            .heap
            .checked_reference(raw, self.types.exclusion_list)
            .map_err(|_| ExecutionError::InvalidExclusionList(value))?;
        self.exclusion_list_contains(list, key)
    }

    fn allocate_pending_copy_data_properties(
        &mut self,
        pending: PendingCopyDataProperties,
    ) -> Result<GcRef<PendingCopyDataProperties>, ExecutionError> {
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
                self.types.pending_copy_data_properties,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates the external source slice retained by Object.assign.
    fn allocate_pending_object_assign(
        &mut self,
        pending: PendingObjectAssign,
    ) -> Result<GcRef<PendingObjectAssign>, ExecutionError> {
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
                self.types.pending_object_assign,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    fn pending_object_assign_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PendingObjectAssign>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_object_assign)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    fn pending_object_assign(
        &mut self,
        state: GcRef<PendingObjectAssign>,
    ) -> Result<PendingObjectAssignSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_object_assign)
                    .map(|pending| PendingObjectAssignSnapshot {
                        target: pending.target,
                        exclusions: pending.exclusions,
                        source: pending.sources.get(pending.index).copied(),
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn advance_pending_object_assign(
        &mut self,
        state: GcRef<PendingObjectAssign>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_object_assign)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending.index = pending.index.saturating_add(1);
                Ok(())
            })
        })
    }

    pub(crate) fn pending_copy_data_properties_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingCopyDataProperties>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_copy_data_properties)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Retrieves the source receiver retained by a pending CopyDataProperties operation.
    pub(crate) fn pending_copy_data_properties_source(
        &mut self,
        state: GcRef<PendingCopyDataProperties>,
    ) -> Result<Value, ExecutionError> {
        self.pending_copy_data_properties(state)
            .map(|pending| pending.source)
    }

    pub(crate) fn pending_copy_data_properties_target(
        &mut self,
        state: GcRef<PendingCopyDataProperties>,
    ) -> Result<Value, ExecutionError> {
        self.pending_copy_data_properties(state)
            .map(|pending| pending.target)
    }

    fn pending_copy_data_properties(
        &mut self,
        state: GcRef<PendingCopyDataProperties>,
    ) -> Result<PendingCopyDataPropertiesSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_copy_data_properties)
                    .map(|pending| PendingCopyDataPropertiesSnapshot {
                        target: pending.target,
                        source: pending.source,
                        exclusions: pending.exclusions,
                        key: pending.keys.get(pending.index).copied(),
                        consumer: pending.consumer,
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn advance_pending_copy_data_properties(
        &mut self,
        state: GcRef<PendingCopyDataProperties>,
    ) -> Result<(), ExecutionError> {
        self.update_pending_copy_index(state, 1)
    }

    fn rewind_pending_copy_data_properties(
        &mut self,
        state: GcRef<PendingCopyDataProperties>,
    ) -> Result<(), ExecutionError> {
        self.update_pending_copy_index(state, -1)
    }

    fn update_pending_copy_index(
        &mut self,
        state: GcRef<PendingCopyDataProperties>,
        delta: isize,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_copy_data_properties)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending.index = pending
                    .index
                    .checked_add_signed(delta)
                    .ok_or(ExecutionError::MissingNativeContinuation)?;
                Ok(())
            })
        })
    }
}

#[derive(Clone, Copy)]
struct PendingCopyDataPropertiesSnapshot {
    target: Value,
    source: Value,
    exclusions: Value,
    key: Option<PropertyKey>,
    consumer: CopyDataPropertiesConsumer,
}

#[derive(Clone, Copy)]
struct PendingObjectAssignSnapshot {
    target: Value,
    exclusions: Value,
    source: Option<Value>,
}
