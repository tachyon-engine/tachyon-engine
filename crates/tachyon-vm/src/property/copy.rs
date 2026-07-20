//! VM-private exclusion lists and ordinary CopyDataProperties fast paths.

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

impl Isolate {
    /// Allocates a VM-private list that cannot invoke user code or inherit user-visible properties.
    pub(crate) fn create_exclusion_list(&mut self, capacity: u32) -> Result<Value, ExecutionError> {
        let capacity =
            usize::try_from(capacity).map_err(|_| ExecutionError::ExclusionListAllocationFailed)?;
        let list = ExclusionList::with_capacity(capacity)?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
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

    /// Copies enumerable own data properties in OrdinaryOwnPropertyKeys order, excluding exact keys.
    pub(crate) fn copy_data_properties(
        &mut self,
        target: Value,
        source: Value,
        exclusions: Value,
    ) -> Result<(), ExecutionError> {
        if matches!(
            source.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(source));
        }
        if !self.is_object_value(source) {
            return Ok(());
        }
        let raw = exclusions
            .as_heap_ref()
            .ok_or(ExecutionError::InvalidExclusionList(exclusions))?;
        let exclusions = self
            .heap
            .checked_reference(raw, self.types.exclusion_list)
            .map_err(|_| ExecutionError::InvalidExclusionList(exclusions))?;
        let (_, snapshot) = self.object_snapshot(source)?;
        let mut keys = self.ordinary_own_property_keys(source, snapshot)?;
        while let Some(entry) = keys.next_entry() {
            if self.exclusion_list_contains(exclusions, entry.key)? {
                continue;
            }
            let Some(property) = entry.property else {
                continue;
            };
            if !property.attributes.enumerable() {
                continue;
            }
            let value = match self.resolve_property_read(source, entry.key)? {
                PropertyRead::Missing => continue,
                PropertyRead::Data(value) => value,
                PropertyRead::Accessor(_) => {
                    return Err(ExecutionError::CopyDataPropertiesNeedsContinuation);
                }
            };
            self.define_data_property(
                target,
                entry.key,
                DataPropertyDescriptor {
                    value: Some(value),
                    writable: Some(true),
                    enumerable: Some(true),
                    configurable: Some(true),
                },
            )?;
        }
        Ok(())
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
}
