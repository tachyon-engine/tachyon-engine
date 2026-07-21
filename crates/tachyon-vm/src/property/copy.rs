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
}

impl Trace for PendingCopyDataProperties {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.target.trace(tracer);
        self.source.trace(tracer);
        self.exclusions.trace(tracer);
        self.keys.trace(tracer);
    }
}

impl GcExternalMemory for PendingCopyDataProperties {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.keys.len() * size_of::<PropertyKey>()
    }
}

impl Isolate {
    /// Allocates a VM-private list that cannot invoke user code or inherit user-visible properties.
    pub(crate) fn create_exclusion_list(&mut self, capacity: u32) -> Result<Value, ExecutionError> {
        let capacity =
            usize::try_from(capacity).map_err(|_| ExecutionError::ExclusionListAllocationFailed)?;
        let list = ExclusionList::with_capacity(capacity)?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
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
        if matches!(
            source.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(source));
        }
        if !self.is_object_value(source) {
            return self
                .write(site.caller_base, site.destination, target)
                .map(|_| None);
        }
        let raw = exclusions
            .as_heap_ref()
            .ok_or(ExecutionError::InvalidExclusionList(exclusions))?;
        let _exclusion_list = self
            .heap
            .checked_reference(raw, self.types.exclusion_list)
            .map_err(|_| ExecutionError::InvalidExclusionList(exclusions))?;
        let (_, snapshot) = self.object_snapshot(source)?;
        let keys = self.ordinary_own_property_keys(source, snapshot)?;
        let mut copied_keys = Vec::new();
        copied_keys
            .try_reserve_exact(keys.len())
            .map_err(|_| ExecutionError::CopyDataPropertiesAllocationFailed)?;
        copied_keys.extend(keys);
        let state = self.allocate_pending_copy_data_properties(PendingCopyDataProperties {
            target,
            source,
            exclusions,
            keys: copied_keys.into_boxed_slice(),
            index: 0,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.advance_copy_data_properties(site, state)
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
        self.copy_data_property(pending.target, key, value)?;
        self.advance_pending_copy_data_properties(state)?;
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
                self.write(site.caller_base, site.destination, pending.target)?;
                return Ok(None);
            };
            self.advance_pending_copy_data_properties(state)?;
            if self.exclusion_list_contains_value(pending.exclusions, key)? {
                continue;
            }
            let (_, snapshot) = self.object_snapshot(pending.source)?;
            let Some(property) = self.shapes.lookup(snapshot.shape, key) else {
                continue;
            };
            if !property.attributes.enumerable()
                || !self.property_is_present_from_snapshot(snapshot, property)?
            {
                continue;
            }
            match self.resolve_property_read(pending.source, key)? {
                PropertyRead::Missing => continue,
                PropertyRead::Data(value) => self.copy_data_property(pending.target, key, value)?,
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    self.copy_data_property(
                        pending.target,
                        key,
                        Value::from_immediate(Immediate::Undefined),
                    )?;
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
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
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
}
