//! Map and Set private-slot operations over fixed-capacity ordered storage.

use super::super::*;
use crate::collection::{
    CollectionEntry, PendingCollectionInitializerRoots, PendingCollectionInitializerSnapshot,
};

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
    #[inline(always)]
    pub(crate) fn is_map_value(&self, value: Value) -> bool {
        value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.map_object)
                .is_ok()
        })
    }

    #[inline(always)]
    pub(crate) fn is_set_value(&self, value: Value) -> bool {
        value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.set_object)
                .is_ok()
        })
    }

    /// Starts Map construction, retaining its initialization record across iterable callbacks.
    pub(crate) fn begin_map_from_site(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let target = self.allocate_map_object(
            self.realm
                .map_prototype
                .expect("Map prototype initializes before Map construction"),
        )?;
        self.begin_collection_initializer(site, target, CollectionInitializerKind::Map)
    }

    /// Starts Set construction, retaining its initialization record across iterable callbacks.
    pub(crate) fn begin_set_from_site(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let target = self.allocate_set_object(
            self.realm
                .set_prototype
                .expect("Set prototype initializes before Set construction"),
        )?;
        self.begin_collection_initializer(site, target, CollectionInitializerKind::Set)
    }

    /// Runs the Map/Set constructor protocol without representing JavaScript calls on the Rust stack.
    fn begin_collection_initializer(
        &mut self,
        site: &CallSite,
        target: Value,
        kind: CollectionInitializerKind,
    ) -> Result<(), ExecutionError> {
        let iterable = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if iterable.as_immediate() == Some(Immediate::Undefined) {
            return self.write(site.caller_base, site.destination, target);
        }
        let state = self.allocate_pending_collection_initializer(PendingCollectionInitializer {
            target,
            iterable,
            iterator: Value::from_immediate(Immediate::Undefined),
            next: Value::from_immediate(Immediate::Undefined),
            result: Value::from_immediate(Immediate::Undefined),
            key: Value::from_immediate(Immediate::Undefined),
            adder: Value::from_immediate(Immediate::Undefined),
            kind,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.get_collection_initializer_property(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            state,
            CollectionInitializerStage::Adder,
            target,
            if kind == CollectionInitializerKind::Map {
                b"set"
            } else {
                b"add"
            },
        )
    }

    /// Resumes one observable iterable-constructor protocol operation from its returned value.
    pub(crate) fn resume_collection_initializer(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCollectionInitializer>,
        stage: CollectionInitializerStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        // The parent completion has been popped before this resume. Re-publish the state before
        // any allocation so local GcRef/Value temporaries never become the only owner.
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        match stage {
            CollectionInitializerStage::Adder => {
                self.update_pending_collection_initializer(state, |pending| pending.adder = value)?;
                self.resolve_function_object(value)?;
                let iterable = self.pending_collection_initializer(state)?.iterable;
                let iterator_symbol = self
                    .realm
                    .well_known_symbols
                    .iterator
                    .expect("Symbol.iterator initializes before collection construction");
                let iterator_key = self.property_key(iterator_symbol)?;
                self.get_collection_initializer_key(
                    site,
                    state,
                    CollectionInitializerStage::IteratorMethod,
                    iterable,
                    iterator_key,
                )
            }
            CollectionInitializerStage::IteratorMethod => {
                self.resolve_function_object(value)?;
                self.update_pending_collection_initializer(state, |pending| pending.next = value)?;
                let pending = self.pending_collection_initializer(state)?;
                self.call_collection_initializer(
                    site,
                    state,
                    CollectionInitializerStage::IteratorCall,
                    pending.next,
                    pending.iterable,
                    &[],
                )
            }
            CollectionInitializerStage::IteratorCall => {
                if !self.is_object_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
                self.update_pending_collection_initializer(state, |pending| {
                    pending.iterator = value
                })?;
                self.get_collection_initializer_property(
                    site,
                    state,
                    CollectionInitializerStage::NextMethod,
                    value,
                    b"next",
                )
            }
            CollectionInitializerStage::NextMethod => {
                self.update_pending_collection_initializer(state, |pending| pending.next = value)?;
                self.resolve_function_object(value)?;
                self.call_collection_next(site, state)
            }
            CollectionInitializerStage::NextCall => {
                if !self.is_object_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
                self.update_pending_collection_initializer(state, |pending| {
                    pending.result = value
                })?;
                self.get_collection_initializer_property(
                    site,
                    state,
                    CollectionInitializerStage::ResultDone,
                    value,
                    b"done",
                )
            }
            CollectionInitializerStage::ResultDone => {
                if self.is_truthy_value(value)? {
                    let target = self.pending_collection_initializer(state)?.target;
                    return self.write(site.caller_base, site.destination, target);
                }
                let result = self.pending_collection_initializer(state)?.result;
                self.get_collection_initializer_property(
                    site,
                    state,
                    CollectionInitializerStage::ResultValue,
                    result,
                    b"value",
                )
            }
            CollectionInitializerStage::ResultValue => {
                let pending = self.pending_collection_initializer(state)?;
                if pending.kind == CollectionInitializerKind::Set {
                    self.update_pending_collection_initializer(state, |pending| {
                        pending.result = value
                    })?;
                    let value = self.pending_collection_initializer(state)?.result;
                    return self.call_collection_adder(site, state, &[value]);
                }
                if !self.is_object_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
                self.update_pending_collection_initializer(state, |pending| {
                    pending.result = value
                })?;
                self.get_collection_initializer_property(
                    site,
                    state,
                    CollectionInitializerStage::EntryKey,
                    value,
                    b"0",
                )
            }
            CollectionInitializerStage::EntryKey => {
                self.update_pending_collection_initializer(state, |pending| pending.key = value)?;
                let entry = self.pending_collection_initializer(state)?.result;
                self.get_collection_initializer_property(
                    site,
                    state,
                    CollectionInitializerStage::EntryValue,
                    entry,
                    b"1",
                )
            }
            CollectionInitializerStage::EntryValue => {
                self.update_pending_collection_initializer(state, |pending| {
                    pending.result = value
                })?;
                let pending = self.pending_collection_initializer(state)?;
                self.call_collection_adder(site, state, &[pending.key, pending.result])
            }
            CollectionInitializerStage::AdderCall => self.call_collection_next(site, state),
        }
    }

    /// Calls the cached iterator `next` without re-reading the method between iterations.
    fn call_collection_next(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCollectionInitializer>,
    ) -> Result<(), ExecutionError> {
        let pending = self.pending_collection_initializer(state)?;
        self.call_collection_initializer(
            site,
            state,
            CollectionInitializerStage::NextCall,
            pending.next,
            pending.iterator,
            &[],
        )
    }

    /// Calls the cached `set` or `add` exactly as the constructor's one-time adder lookup requires.
    fn call_collection_adder(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCollectionInitializer>,
        arguments: &[Value],
    ) -> Result<(), ExecutionError> {
        let pending = self.pending_collection_initializer(state)?;
        self.call_collection_initializer(
            site,
            state,
            CollectionInitializerStage::AdderCall,
            pending.adder,
            pending.target,
            arguments,
        )
    }

    /// Reads one named protocol property and suspends only when it is an accessor callback.
    fn get_collection_initializer_property(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCollectionInitializer>,
        stage: CollectionInitializerStage,
        receiver: Value,
        name: &[u8],
    ) -> Result<(), ExecutionError> {
        let key = PropertyKey::Atom(self.intern_intrinsic_name(name)?);
        self.get_collection_initializer_key(site, state, stage, receiver, key)
    }

    /// Reads one arbitrary protocol key, preserving ordinary accessor receiver semantics.
    fn get_collection_initializer_key(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCollectionInitializer>,
        stage: CollectionInitializerStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        match self.resolve_property_read(receiver, key)? {
            PropertyRead::Data(value) => {
                self.resume_collection_initializer(site, state, stage, value)
            }
            PropertyRead::Missing => self.resume_collection_initializer(
                site,
                state,
                stage,
                Value::from_immediate(Immediate::Undefined),
            ),
            PropertyRead::Accessor(callee)
                if callee.as_immediate() == Some(Immediate::Undefined) =>
            {
                self.resume_collection_initializer(
                    site,
                    state,
                    stage,
                    Value::from_immediate(Immediate::Undefined),
                )
            }
            PropertyRead::Accessor(callee) => {
                self.call_collection_initializer(site, state, stage, callee, receiver, &[])
            }
        }
    }

    /// Publishes the collection state before a protocol call and handles immediate native results.
    fn call_collection_initializer(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCollectionInitializer>,
        stage: CollectionInitializerStage,
        callee: Value,
        receiver: Value,
        arguments: &[Value],
    ) -> Result<(), ExecutionError> {
        let mut copied_arguments = Vec::new();
        copied_arguments
            .try_reserve_exact(arguments.len())
            .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)?;
        copied_arguments.extend_from_slice(arguments);
        let prefix = if copied_arguments.is_empty() {
            None
        } else {
            Some(self.create_apply_argument_prefix(callee, receiver, copied_arguments)?)
        };
        let continuation = NativeContinuation::collection_initializer(
            site,
            stage,
            Value::from_heap_ref(state.raw()),
            callee,
        );
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(|_| ExecutionError::CompletionAllocationFailed)?;
        let frame_depth = self.fiber.frames.len();
        let call_result = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee,
            argument_base: 0,
            argument_prefix: prefix,
            argument_prefix_offset: 0,
            argument_prefix_count: u32::try_from(arguments.len())
                .map_err(|_| ExecutionError::BoundArgumentCountOverflow)?,
            argument_count: u32::try_from(arguments.len())
                .map_err(|_| ExecutionError::BoundArgumentCountOverflow)?,
            this_value: receiver,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        });
        if let Err(error) = call_result {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("a collection protocol call publishes its callee frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let returned = self.read(site.caller_base, site.destination)?;
        let _continuation = self.pop_native_continuation()?;
        self.resume_collection_initializer(site, state, stage, returned)
    }

    /// Allocates the small traced protocol record used by resumable collection construction.
    fn allocate_pending_collection_initializer(
        &mut self,
        pending: PendingCollectionInitializer,
    ) -> Result<GcRef<PendingCollectionInitializer>, ExecutionError> {
        let mut roots = PendingCollectionInitializerRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            pending,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_collection_initializer,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn pending_collection_initializer_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingCollectionInitializer>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_collection_initializer)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    fn pending_collection_initializer(
        &mut self,
        state: GcRef<PendingCollectionInitializer>,
    ) -> Result<PendingCollectionInitializerSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_collection_initializer)
                    .map(|pending| PendingCollectionInitializerSnapshot {
                        target: pending.target,
                        iterable: pending.iterable,
                        iterator: pending.iterator,
                        next: pending.next,
                        result: pending.result,
                        key: pending.key,
                        adder: pending.adder,
                        kind: pending.kind,
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn update_pending_collection_initializer(
        &mut self,
        state: GcRef<PendingCollectionInitializer>,
        update: impl FnOnce(&mut PendingCollectionInitializer),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_collection_initializer)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
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

    /// Returns an existing Map value or inserts the supplied default with one SameValueZero probe.
    pub(crate) fn map_get_or_insert(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let key = self.call_argument(site, 0)?;
        let key = self.collection_key(key);
        let default = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let storage = self.map_storage(site.this_value)?;
        if let Some(index) = self.collection_find(storage, key)? {
            return self
                .collection_entry(storage, index)?
                .map(|entry| entry.value)
                .ok_or(ExecutionError::CollectionStorageAllocationFailed);
        }
        let storage = self.ensure_map_capacity(site.this_value, storage)?;
        self.collection_append(storage, key, default)?;
        Ok(default)
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

    /// Creates a Map keys iterator retaining live insertion-order state.
    pub(crate) fn map_keys(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        self.map_storage(receiver)?;
        self.create_collection_iterator(receiver, CollectionIterationKind::Key, true)
    }

    /// Creates a Map values iterator retaining live insertion-order state.
    pub(crate) fn map_values(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        self.map_storage(receiver)?;
        self.create_collection_iterator(receiver, CollectionIterationKind::Value, true)
    }

    /// Creates a Map entries iterator retaining live insertion-order state.
    pub(crate) fn map_entries(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        self.map_storage(receiver)?;
        self.create_collection_iterator(receiver, CollectionIterationKind::KeyAndValue, true)
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

    /// Creates a Set values iterator retaining live insertion-order state.
    pub(crate) fn set_values(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        self.set_storage(receiver)?;
        self.create_collection_iterator(receiver, CollectionIterationKind::Value, false)
    }

    /// Creates a Set entries iterator yielding `[value, value]` pairs.
    pub(crate) fn set_entries(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        self.set_storage(receiver)?;
        self.create_collection_iterator(receiver, CollectionIterationKind::KeyAndValue, false)
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

    pub(crate) fn map_storage(
        &mut self,
        receiver: Value,
    ) -> Result<GcRef<OrderedCollection>, ExecutionError> {
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

    pub(crate) fn set_storage(
        &mut self,
        receiver: Value,
    ) -> Result<GcRef<OrderedCollection>, ExecutionError> {
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

    pub(crate) fn collection_entry(
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

    pub(crate) fn collection_used(
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
