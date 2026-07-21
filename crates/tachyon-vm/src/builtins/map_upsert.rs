//! Resumable Map upsert callbacks that cannot execute JavaScript on the Rust stack.

use tachyon_gc::{AllocationSpace, GcRef, Trace, Tracer};
use tachyon_value::{Immediate, Value};

use crate::{
    CallSite, ExecutionError, Isolate, NativeContinuation, NativeContinuationSite,
    PendingMapGetOrInsertComputed, VmRoots,
};

struct MapUpsertRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingMapGetOrInsertComputed,
}

impl Trace for MapUpsertRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Validates the callback before probing and calls it only when the canonical key is absent.
    pub(crate) fn begin_map_get_or_insert_computed(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let storage = self.map_storage(site.this_value)?;
        let key = self.call_argument(site, 0)?;
        let key = self.collection_key(key);
        let callback = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.resolve_function_object(callback)?;
        if let Some(index) = self.collection_find(storage, key)? {
            return self
                .collection_entry(storage, index)?
                .map(|entry| self.write(site.caller_base, site.destination, entry.value))
                .transpose()?
                .ok_or(ExecutionError::CollectionStorageAllocationFailed);
        }
        let state = self.allocate_pending_map_upsert(PendingMapGetOrInsertComputed {
            map: site.this_value,
            key,
            callback,
            weak: false,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.call_map_upsert_callback(continuation_site, state)
    }

    /// Validates a weak key and starts the same callback continuation over ephemeron storage.
    pub(crate) fn begin_weak_map_get_or_insert_computed(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let storage = self.weak_map_storage(site.this_value)?;
        let key = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let key = self.weak_key(key)?;
        let callback = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.resolve_function_object(callback)?;
        let storage = storage.ok_or(ExecutionError::IncompatibleCollectionReceiver(
            site.this_value,
        ))?;
        if let Some(index) = self.weak_collection_find(storage, key)? {
            return self
                .weak_collection_entry(storage, index)?
                .map(|entry| self.write(site.caller_base, site.destination, entry.value()))
                .transpose()?
                .ok_or(ExecutionError::CollectionStorageAllocationFailed);
        }
        let state = self.allocate_pending_map_upsert(PendingMapGetOrInsertComputed {
            map: site.this_value,
            key,
            callback,
            weak: true,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.call_map_upsert_callback(continuation_site, state)
    }

    /// Resumes after a callback and overwrites any same-key mutation with its returned value.
    pub(crate) fn resume_map_get_or_insert_computed(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingMapGetOrInsertComputed>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.pending_map_upsert(state)?;
        if pending.weak {
            let storage = self
                .weak_map_storage(pending.map)?
                .ok_or(ExecutionError::IncompatibleCollectionReceiver(pending.map))?;
            self.weak_collection_set(pending.map, storage, pending.key, value, true)?;
        } else {
            let storage = self.map_storage(pending.map)?;
            if let Some(index) = self.collection_find(storage, pending.key)? {
                self.collection_update(storage, index, value)?;
            } else {
                let storage = self.ensure_map_capacity(pending.map, storage)?;
                self.collection_append(storage, pending.key, value)?;
            }
        }
        self.write(site.caller_base, site.destination, value)
    }

    /// Calls the cached callback through the normal VM call path and retains typed completion state.
    fn call_map_upsert_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingMapGetOrInsertComputed>,
    ) -> Result<(), ExecutionError> {
        let pending = self.pending_map_upsert(state)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let prefix = self.create_apply_argument_prefix(
            pending.callback,
            Value::from_immediate(Immediate::Undefined),
            vec![pending.key],
        )?;
        self.fiber
            .completions
            .push_native(NativeContinuation::map_get_or_insert_computed(
                site,
                Value::from_heap_ref(state.raw()),
                pending.callback,
            ))
            .map_err(|_| ExecutionError::CompletionAllocationFailed)?;
        let frame_depth = self.fiber.frames.len();
        let result = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: pending.callback,
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: 1,
            argument_count: 1,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        });
        if let Err(error) = result {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("upsert callback publishes a callee frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        self.pop_native_continuation()?;
        self.resume_map_get_or_insert_computed(
            site,
            state,
            self.read(site.caller_base, site.destination)?,
        )
    }

    fn allocate_pending_map_upsert(
        &mut self,
        pending: PendingMapGetOrInsertComputed,
    ) -> Result<GcRef<PendingMapGetOrInsertComputed>, ExecutionError> {
        let mut roots = MapUpsertRoots {
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
                self.types.pending_map_get_or_insert_computed,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn pending_map_upsert_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingMapGetOrInsertComputed>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_map_get_or_insert_computed)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    fn pending_map_upsert(
        &mut self,
        state: GcRef<PendingMapGetOrInsertComputed>,
    ) -> Result<PendingMapGetOrInsertComputed, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_map_get_or_insert_computed)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }
}
