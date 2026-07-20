//! Map and Set private-slot operations over fixed-capacity ordered storage.

use super::super::*;
use crate::collection::CollectionEntry;

struct CollectionReplacementRoots<'a> {
    vm: VmRoots<'a>,
    receiver: Value,
    storage: GcRef<OrderedCollection>,
}

impl Trace for CollectionReplacementRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.receiver.trace(tracer);
        self.storage.trace(tracer);
    }
}

impl Isolate {
    /// Creates an empty Map; iterable initialization joins the resumable iterator-protocol slice.
    pub(crate) fn create_map_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        if site.argument_count != 0 {
            return Err(ExecutionError::UnsupportedCollectionInitializer);
        }
        self.allocate_map_object(
            self.realm
                .map_prototype
                .expect("Map prototype initializes before Map construction"),
        )
    }

    /// Creates an empty Set; iterable initialization joins the resumable iterator-protocol slice.
    pub(crate) fn create_set_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        if site.argument_count != 0 {
            return Err(ExecutionError::UnsupportedCollectionInitializer);
        }
        self.allocate_set_object(
            self.realm
                .set_prototype
                .expect("Set prototype initializes before Set construction"),
        )
    }

    /// Implements Map.prototype.get using SameValueZero over the exotic's private insertion list.
    pub(crate) fn map_get(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        let key = self.collection_key(argument);
        let storage = self.map_storage(site.this_value)?;
        let value = match self.collection_find(storage, key)? {
            Some(index) => self
                .collection_entry(storage, index)?
                .map(|entry| entry.value),
            None => None,
        };
        Ok(value.unwrap_or(Value::from_immediate(Immediate::Undefined)))
    }

    /// Implements Map.prototype.set, including canonical -0 keys and replacement backing growth.
    pub(crate) fn map_set(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        let key = self.collection_key(argument);
        let value = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let storage = self.map_storage(site.this_value)?;
        if let Some(index) = self.collection_find(storage, key)? {
            self.collection_update(storage, index, value)?;
        } else {
            let storage = self.ensure_map_capacity(site.this_value, storage)?;
            self.collection_append(storage, key, value)?;
        }
        Ok(site.this_value)
    }

    /// Implements Map.prototype.has without materializing a result-array or iterator.
    pub(crate) fn map_has(&mut self, site: &CallSite) -> Result<bool, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        let key = self.collection_key(argument);
        let storage = self.map_storage(site.this_value)?;
        self.collection_find(storage, key)
            .map(|entry| entry.is_some())
    }

    /// Implements Map.prototype.delete while retaining a tombstone for live iterator cursors.
    pub(crate) fn map_delete(&mut self, site: &CallSite) -> Result<bool, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        let key = self.collection_key(argument);
        let storage = self.map_storage(site.this_value)?;
        let Some(index) = self.collection_find(storage, key)? else {
            return Ok(false);
        };
        self.collection_delete(storage, index)?;
        Ok(true)
    }

    /// Implements Map.prototype.clear without shortening the physical cursor list.
    pub(crate) fn map_clear(&mut self, receiver: Value) -> Result<(), ExecutionError> {
        let storage = self.map_storage(receiver)?;
        self.collection_clear(storage)
    }

    /// Reads Map.prototype.size from the private live-entry count.
    pub(crate) fn map_size(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        let storage = self.map_storage(receiver)?;
        Ok(safe_integer_value(u64::from(self.collection_len(storage)?)))
    }

    /// Implements Set.prototype.add by storing each member as both ordered key and value.
    pub(crate) fn set_add(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        let value = self.collection_key(argument);
        let storage = self.set_storage(site.this_value)?;
        if self.collection_find(storage, value)?.is_none() {
            let storage = self.ensure_set_capacity(site.this_value, storage)?;
            self.collection_append(storage, value, value)?;
        }
        Ok(site.this_value)
    }

    /// Implements Set.prototype.has with the same canonicalized SameValueZero key path as add.
    pub(crate) fn set_has(&mut self, site: &CallSite) -> Result<bool, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        let value = self.collection_key(argument);
        let storage = self.set_storage(site.this_value)?;
        self.collection_find(storage, value)
            .map(|entry| entry.is_some())
    }

    /// Implements Set.prototype.delete while preserving physical positions for future iterators.
    pub(crate) fn set_delete(&mut self, site: &CallSite) -> Result<bool, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        let value = self.collection_key(argument);
        let storage = self.set_storage(site.this_value)?;
        let Some(index) = self.collection_find(storage, value)? else {
            return Ok(false);
        };
        self.collection_delete(storage, index)?;
        Ok(true)
    }

    /// Implements Set.prototype.clear without shrinking the fixed GC-accounted backing.
    pub(crate) fn set_clear(&mut self, receiver: Value) -> Result<(), ExecutionError> {
        let storage = self.set_storage(receiver)?;
        self.collection_clear(storage)
    }

    /// Reads Set.prototype.size from the private live-entry count.
    pub(crate) fn set_size(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        let storage = self.set_storage(receiver)?;
        Ok(safe_integer_value(u64::from(self.collection_len(storage)?)))
    }

    #[inline(always)]
    fn collection_key(&self, value: Option<Value>) -> Value {
        let value = value.unwrap_or(Value::from_immediate(Immediate::Undefined));
        if numeric_value(value).is_some_and(|number| number == 0.0) {
            Value::from_f64(0.0)
        } else {
            value
        }
    }

    fn map_storage(&mut self, receiver: Value) -> Result<GcRef<OrderedCollection>, ExecutionError> {
        let raw = receiver
            .as_heap_ref()
            .ok_or(ExecutionError::IncompatibleCollectionReceiver(receiver))?;
        let map = self
            .heap
            .checked_reference(raw, self.types.map_object)
            .map_err(|_| ExecutionError::IncompatibleCollectionReceiver(receiver))?;
        self.heap.with_running_scope(|scope| {
            let map = scope.root(map).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(map, self.types.map_object)
                    .map(|map| map.storage)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn set_storage(&mut self, receiver: Value) -> Result<GcRef<OrderedCollection>, ExecutionError> {
        let raw = receiver
            .as_heap_ref()
            .ok_or(ExecutionError::IncompatibleCollectionReceiver(receiver))?;
        let set = self
            .heap
            .checked_reference(raw, self.types.set_object)
            .map_err(|_| ExecutionError::IncompatibleCollectionReceiver(receiver))?;
        self.heap.with_running_scope(|scope| {
            let set = scope.root(set).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(set, self.types.set_object)
                    .map(|set| set.storage)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Scans physical entries with borrow-free SameValueZero comparisons between reads.
    fn collection_find(
        &mut self,
        storage: GcRef<OrderedCollection>,
        key: Value,
    ) -> Result<Option<u32>, ExecutionError> {
        let used = self.collection_used(storage)?;
        for index in 0..used {
            if let Some(entry) = self.collection_entry(storage, index)?
                && self.same_value_zero(entry.key, key)?
            {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }

    fn collection_entry(
        &mut self,
        storage: GcRef<OrderedCollection>,
        index: u32,
    ) -> Result<Option<CollectionEntry>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(storage, self.types.ordered_collection)
                    .map(|storage| storage.entry_at(index))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn collection_used(
        &mut self,
        storage: GcRef<OrderedCollection>,
    ) -> Result<u32, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(storage, self.types.ordered_collection)
                    .map(|storage| storage.used())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn collection_len(&mut self, storage: GcRef<OrderedCollection>) -> Result<u32, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(storage, self.types.ordered_collection)
                    .map(|storage| storage.len())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn collection_update(
        &mut self,
        storage: GcRef<OrderedCollection>,
        index: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(storage, self.types.ordered_collection)
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

    fn collection_append(
        &mut self,
        storage: GcRef<OrderedCollection>,
        key: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(storage, self.types.ordered_collection)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .append(key, value)
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

    fn collection_delete(
        &mut self,
        storage: GcRef<OrderedCollection>,
        index: u32,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let storage = no_gc
                    .borrow_mut(storage, self.types.ordered_collection)
                    .map_err(ExecutionError::NoGcBorrow)?;
                storage
                    .delete_at(index)
                    .map_err(|_| ExecutionError::CollectionStorageAllocationFailed)
            })
        })
    }

    fn collection_clear(
        &mut self,
        storage: GcRef<OrderedCollection>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(storage, self.types.ordered_collection)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .clear();
                Ok(())
            })
        })
    }

    fn ensure_map_capacity(
        &mut self,
        receiver: Value,
        storage: GcRef<OrderedCollection>,
    ) -> Result<GcRef<OrderedCollection>, ExecutionError> {
        self.ensure_collection_capacity(receiver, storage, true)
    }

    fn ensure_set_capacity(
        &mut self,
        receiver: Value,
        storage: GcRef<OrderedCollection>,
    ) -> Result<GcRef<OrderedCollection>, ExecutionError> {
        self.ensure_collection_capacity(receiver, storage, false)
    }

    /// Allocates an exactly charged replacement backing and publishes it into the owning exotic.
    fn ensure_collection_capacity(
        &mut self,
        receiver: Value,
        storage: GcRef<OrderedCollection>,
        map: bool,
    ) -> Result<GcRef<OrderedCollection>, ExecutionError> {
        let (used, capacity) = self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(storage, self.types.ordered_collection)
                    .map(|storage| (storage.used(), storage.capacity()))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        if (used as usize) < capacity {
            return Ok(storage);
        }
        let capacity = tuning::collections::grown_entry_capacity(capacity)
            .ok_or(ExecutionError::CollectionStorageAllocationFailed)?;
        let replacement = self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(storage, self.types.ordered_collection)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .grow_copy(capacity)
                    .map_err(|_| ExecutionError::CollectionStorageAllocationFailed)
            })
        })?;
        let mut roots = CollectionReplacementRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            receiver,
            storage,
        };
        let replacement = self
            .heap
            .try_allocate_external_with_gc(
                self.types.ordered_collection,
                0,
                replacement,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let raw = receiver
            .as_heap_ref()
            .ok_or(ExecutionError::IncompatibleCollectionReceiver(receiver))?;
        if map {
            let object = self
                .heap
                .checked_reference(raw, self.types.map_object)
                .map_err(|_| ExecutionError::IncompatibleCollectionReceiver(receiver))?;
            self.heap.with_running_scope(|scope| {
                let object = scope.root(object).map_err(ExecutionError::Root)?;
                let replacement = scope.root(replacement).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(object, self.types.map_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .storage = replacement.as_gc_ref();
                    Ok(())
                })?;
                scope
                    .write_barrier(object, replacement)
                    .map_err(ExecutionError::HeapReference)?;
                Ok(())
            })?;
        } else {
            let object = self
                .heap
                .checked_reference(raw, self.types.set_object)
                .map_err(|_| ExecutionError::IncompatibleCollectionReceiver(receiver))?;
            self.heap.with_running_scope(|scope| {
                let object = scope.root(object).map_err(ExecutionError::Root)?;
                let replacement = scope.root(replacement).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(object, self.types.set_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .storage = replacement.as_gc_ref();
                    Ok(())
                })?;
                scope
                    .write_barrier(object, replacement)
                    .map_err(ExecutionError::HeapReference)?;
                Ok(())
            })?;
        }
        Ok(replacement)
    }
}
