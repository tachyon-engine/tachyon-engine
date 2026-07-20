//! WeakMap and WeakSet operations backed by GC ephemerons.

use super::super::*;
use tachyon_gc::Ephemeron;

impl Isolate {
    /// Looks up a WeakMap entry, returning undefined for keys that cannot be held weakly.
    pub(crate) fn weak_map_get(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let key = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let Some(storage) = self.weak_map_storage(site.this_value)? else {
            return Ok(Value::from_immediate(Immediate::Undefined));
        };
        let Some(index) = self.weak_collection_find(storage, key)? else {
            return Ok(Value::from_immediate(Immediate::Undefined));
        };
        Ok(self
            .weak_collection_entry(storage, index)?
            .map_or(Value::from_immediate(Immediate::Undefined), |entry| {
                entry.value()
            }))
    }

    /// Implements WeakMap.prototype.set with an ephemeron conditional value edge.
    pub(crate) fn weak_map_set(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let key = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let key = self.weak_key(key)?;
        let value = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let storage = self.weak_map_storage(site.this_value)?.ok_or(
            ExecutionError::IncompatibleCollectionReceiver(site.this_value),
        )?;
        self.weak_collection_set(site.this_value, storage, key, value, true)?;
        Ok(site.this_value)
    }

    pub(crate) fn weak_map_has(&mut self, site: &CallSite) -> Result<bool, ExecutionError> {
        let key = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        Ok(self
            .weak_map_storage(site.this_value)?
            .map(|storage| self.weak_collection_find(storage, key))
            .transpose()?
            .flatten()
            .is_some())
    }

    pub(crate) fn weak_map_delete(&mut self, site: &CallSite) -> Result<bool, ExecutionError> {
        let key = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let Some(storage) = self.weak_map_storage(site.this_value)? else {
            return Ok(false);
        };
        let Some(index) = self.weak_collection_find(storage, key)? else {
            return Ok(false);
        };
        self.weak_collection_delete(storage, index)?;
        Ok(true)
    }

    /// Implements WeakSet.prototype.add by storing the key as its own ephemeron value.
    pub(crate) fn weak_set_add(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let key = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let key = self.weak_key(key)?;
        let storage = self.weak_set_storage(site.this_value)?.ok_or(
            ExecutionError::IncompatibleCollectionReceiver(site.this_value),
        )?;
        self.weak_collection_set(site.this_value, storage, key, key, false)?;
        Ok(site.this_value)
    }

    pub(crate) fn weak_set_has(&mut self, site: &CallSite) -> Result<bool, ExecutionError> {
        let key = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        Ok(self
            .weak_set_storage(site.this_value)?
            .map(|storage| self.weak_collection_find(storage, key))
            .transpose()?
            .flatten()
            .is_some())
    }

    pub(crate) fn weak_set_delete(&mut self, site: &CallSite) -> Result<bool, ExecutionError> {
        let key = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let Some(storage) = self.weak_set_storage(site.this_value)? else {
            return Ok(false);
        };
        let Some(index) = self.weak_collection_find(storage, key)? else {
            return Ok(false);
        };
        self.weak_collection_delete(storage, index)?;
        Ok(true)
    }

    fn weak_key(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if self.is_weak_key(value)? {
            Ok(value)
        } else {
            Err(ExecutionError::NotObject(value))
        }
    }

    #[inline(always)]
    fn is_weak_key(&mut self, value: Value) -> Result<bool, ExecutionError> {
        if self.is_object_value(value) {
            return Ok(true);
        }
        if !self.is_symbol_value(value) {
            return Ok(false);
        }
        self.is_registered_symbol(value)
            .map(|registered| !registered)
    }

    fn weak_map_storage(
        &mut self,
        receiver: Value,
    ) -> Result<Option<GcRef<WeakCollection>>, ExecutionError> {
        self.weak_storage(receiver, self.types.weak_map_object, |object| {
            object.storage
        })
    }

    fn weak_set_storage(
        &mut self,
        receiver: Value,
    ) -> Result<Option<GcRef<WeakCollection>>, ExecutionError> {
        self.weak_storage(receiver, self.types.weak_set_object, |object| {
            object.storage
        })
    }

    fn weak_storage<T: Trace + 'static>(
        &mut self,
        receiver: Value,
        ty: GcType<T>,
        storage: impl FnOnce(&T) -> GcRef<WeakCollection>,
    ) -> Result<Option<GcRef<WeakCollection>>, ExecutionError> {
        let Some(raw) = receiver.as_heap_ref() else {
            return Err(ExecutionError::IncompatibleCollectionReceiver(receiver));
        };
        let Ok(object) = self.heap.checked_reference(raw, ty) else {
            return Err(ExecutionError::IncompatibleCollectionReceiver(receiver));
        };
        self.heap.with_running_scope(|scope| {
            let object = scope.root(object).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(object, ty)
                    .map(|object| Some(storage(object)))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn weak_collection_find(
        &mut self,
        storage: GcRef<WeakCollection>,
        key: Value,
    ) -> Result<Option<usize>, ExecutionError> {
        if !self.is_weak_key(key)? {
            return Ok(None);
        }
        let raw = key.as_heap_ref().expect("weak key was checked above");
        let capacity = self.weak_collection_capacity(storage)?;
        for index in 0..capacity {
            if self
                .weak_collection_entry(storage, index)?
                .and_then(|entry| entry.key())
                .is_some_and(|current| current.raw() == raw)
            {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }

    fn weak_collection_set(
        &mut self,
        receiver: Value,
        storage: GcRef<WeakCollection>,
        key: Value,
        value: Value,
        map: bool,
    ) -> Result<(), ExecutionError> {
        if let Some(index) = self.weak_collection_find(storage, key)? {
            return self.weak_collection_update(storage, index, value);
        }
        let storage = self.ensure_weak_collection_capacity(receiver, storage, map)?;
        let index = self.weak_collection_free_slot(storage)?;
        let raw = key
            .as_heap_ref()
            .expect("weak key was checked before insertion");
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(storage, self.types.weak_collection)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .install_at(index, Ephemeron::new(GcRef::from_erased_raw(raw), value))
                    .map_err(|_| ExecutionError::CollectionStorageAllocationFailed)
            })?;
            scope
                .write_value_barrier(storage, key)
                .map_err(ExecutionError::HeapReference)?;
            scope
                .write_value_barrier(storage, value)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    fn weak_collection_entry(
        &mut self,
        storage: GcRef<WeakCollection>,
        index: usize,
    ) -> Result<Option<Ephemeron<()>>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(storage, self.types.weak_collection)
                    .map(|table| table.entry_at(index))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn weak_collection_capacity(
        &mut self,
        storage: GcRef<WeakCollection>,
    ) -> Result<usize, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(storage, self.types.weak_collection)
                    .map(|table| table.capacity())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn weak_collection_free_slot(
        &mut self,
        storage: GcRef<WeakCollection>,
    ) -> Result<usize, ExecutionError> {
        (0..self.weak_collection_capacity(storage)?)
            .find(|index| {
                self.weak_collection_entry(storage, *index)
                    .ok()
                    .flatten()
                    .is_none_or(|entry| entry.key().is_none())
            })
            .ok_or(ExecutionError::CollectionStorageAllocationFailed)
    }

    fn weak_collection_update(
        &mut self,
        storage: GcRef<WeakCollection>,
        index: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(storage, self.types.weak_collection)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .update_at(index, value)
                    .map_err(|_| ExecutionError::CollectionStorageAllocationFailed)
            })?;
            scope
                .write_value_barrier(storage, value)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    fn weak_collection_delete(
        &mut self,
        storage: GcRef<WeakCollection>,
        index: usize,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(storage, self.types.weak_collection)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .delete_at(index)
                    .map_err(|_| ExecutionError::CollectionStorageAllocationFailed)
            })
        })
    }

    fn ensure_weak_collection_capacity(
        &mut self,
        receiver: Value,
        storage: GcRef<WeakCollection>,
        map: bool,
    ) -> Result<GcRef<WeakCollection>, ExecutionError> {
        if self.weak_collection_free_slot(storage).is_ok() {
            return Ok(storage);
        }
        let capacity =
            tuning::collections::grown_entry_capacity(self.weak_collection_capacity(storage)?)
                .ok_or(ExecutionError::CollectionStorageAllocationFailed)?;
        let replacement = self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(storage, self.types.weak_collection)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .grow_copy(capacity)
                    .map_err(|_| ExecutionError::CollectionStorageAllocationFailed)
            })
        })?;
        let replacement = self
            .heap
            .try_allocate_external_with_gc(
                self.types.weak_collection,
                0,
                replacement,
                AllocationSpace::Young,
                &mut VmRoots {
                    fiber: &mut self.fiber,
                    finalization_jobs: &mut self.finalization_jobs,
                    realm: &mut self.realm,
                    loaded_code: &mut self.loaded_code,
                },
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let raw = receiver
            .as_heap_ref()
            .ok_or(ExecutionError::IncompatibleCollectionReceiver(receiver))?;
        if map {
            let object = self
                .heap
                .checked_reference(raw, self.types.weak_map_object)
                .map_err(|_| ExecutionError::IncompatibleCollectionReceiver(receiver))?;
            self.heap.with_running_scope(|scope| {
                let object = scope.root(object).map_err(ExecutionError::Root)?;
                let replacement = scope.root(replacement).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(object, self.types.weak_map_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .storage = replacement.as_gc_ref();
                    Ok::<(), ExecutionError>(())
                })?;
                scope
                    .write_barrier(object, replacement)
                    .map_err(ExecutionError::HeapReference)
            })?;
        } else {
            let object = self
                .heap
                .checked_reference(raw, self.types.weak_set_object)
                .map_err(|_| ExecutionError::IncompatibleCollectionReceiver(receiver))?;
            self.heap.with_running_scope(|scope| {
                let object = scope.root(object).map_err(ExecutionError::Root)?;
                let replacement = scope.root(replacement).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(object, self.types.weak_set_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .storage = replacement.as_gc_ref();
                    Ok::<(), ExecutionError>(())
                })?;
                scope
                    .write_barrier(object, replacement)
                    .map_err(ExecutionError::HeapReference)
            })?;
        }
        Ok(replacement)
    }
}
