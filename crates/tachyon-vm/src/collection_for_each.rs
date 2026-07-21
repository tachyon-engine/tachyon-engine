//! Resumable live scans for Map and Set `forEach` callbacks.

use tachyon_gc::{AllocationSpace, GcRef, Trace, Tracer};
use tachyon_value::{Immediate, Value};

use crate::collection::CollectionEntry;
use crate::{
    CallSite, ExecutionError, Isolate, NativeContinuation, NativeContinuationSite,
    PendingCollectionForEach, VmRoots,
};

struct CollectionForEachRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingCollectionForEach,
}

impl Trace for CollectionForEachRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

#[derive(Clone, Copy)]
struct CollectionForEachSnapshot {
    collection: Value,
    callback: Value,
    this_argument: Value,
    value: Value,
    key: Value,
    next_index: u32,
    map: bool,
}

impl Isolate {
    /// Starts a live Map/Set forEach scan after validating the branded receiver and callback.
    pub(crate) fn begin_collection_for_each(
        &mut self,
        site: &CallSite,
        map: bool,
    ) -> Result<(), ExecutionError> {
        let collection = site.this_value;
        if map {
            self.map_storage(collection)?;
        } else {
            self.set_storage(collection)?;
        }
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.resolve_function_object(callback)?;
        let this_argument = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let state = self.allocate_pending_collection_for_each(PendingCollectionForEach {
            collection,
            callback,
            this_argument,
            value: Value::from_immediate(Immediate::Undefined),
            key: Value::from_immediate(Immediate::Undefined),
            next_index: 0,
            map,
        })?;
        let site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.advance_collection_for_each(site, state)
    }

    /// Resumes a forEach scan after a callback's normal completion without observing its result.
    pub(crate) fn resume_collection_for_each(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCollectionForEach>,
    ) -> Result<(), ExecutionError> {
        self.advance_collection_for_each(site, state)
    }

    /// Scans tombstones synchronously and publishes exactly one JavaScript callback at a time.
    fn advance_collection_for_each(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCollectionForEach>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        loop {
            let pending = self.pending_collection_for_each(state)?;
            let storage = if pending.map {
                self.map_storage(pending.collection)?
            } else {
                self.set_storage(pending.collection)?
            };
            let used = self.collection_used(storage)?;
            if pending.next_index >= used {
                return self.write(
                    site.caller_base,
                    site.destination,
                    Value::from_immediate(Immediate::Undefined),
                );
            }
            self.update_collection_for_each(state, |pending| {
                pending.next_index += 1;
            })?;
            let Some(entry) = self.collection_entry(storage, pending.next_index)? else {
                continue;
            };
            self.set_collection_for_each_entry(state, entry)?;
            return self.call_collection_for_each_callback(site, state);
        }
    }

    /// Publishes callback state before calling the user function with the exact three arguments.
    fn call_collection_for_each_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCollectionForEach>,
    ) -> Result<(), ExecutionError> {
        let pending = self.pending_collection_for_each(state)?;
        let arguments = if pending.map {
            vec![pending.value, pending.key, pending.collection]
        } else {
            vec![pending.value, pending.value, pending.collection]
        };
        let prefix =
            self.create_apply_argument_prefix(pending.callback, pending.this_argument, arguments)?;
        let continuation = NativeContinuation::collection_for_each(
            site,
            Value::from_heap_ref(state.raw()),
            pending.callback,
        );
        self.fiber
            .completions
            .push_native(continuation)
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
            argument_prefix_count: 3,
            argument_count: 3,
            this_value: pending.this_argument,
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
                .expect("a forEach callback publishes its callee frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        self.pop_native_continuation()?;
        self.advance_collection_for_each(site, state)
    }

    /// Allocates the traced state that remains live across a callback allocation or collection.
    fn allocate_pending_collection_for_each(
        &mut self,
        pending: PendingCollectionForEach,
    ) -> Result<GcRef<PendingCollectionForEach>, ExecutionError> {
        let mut roots = CollectionForEachRoots {
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
                self.types.pending_collection_for_each,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn pending_collection_for_each_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingCollectionForEach>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_collection_for_each)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    fn pending_collection_for_each(
        &mut self,
        state: GcRef<PendingCollectionForEach>,
    ) -> Result<CollectionForEachSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_collection_for_each)
                    .map(|pending| CollectionForEachSnapshot {
                        collection: pending.collection,
                        callback: pending.callback,
                        this_argument: pending.this_argument,
                        value: pending.value,
                        key: pending.key,
                        next_index: pending.next_index,
                        map: pending.map,
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn update_collection_for_each(
        &mut self,
        state: GcRef<PendingCollectionForEach>,
        update: impl FnOnce(&mut PendingCollectionForEach),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                update(
                    no_gc
                        .borrow_mut(state, self.types.pending_collection_for_each)
                        .map_err(ExecutionError::NoGcBorrow)?,
                );
                Ok(())
            })
        })
    }

    /// Stores callback arguments with barriers because the pending state may have promoted.
    fn set_collection_for_each_entry(
        &mut self,
        state: GcRef<PendingCollectionForEach>,
        entry: CollectionEntry,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_collection_for_each)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending.value = entry.value;
                pending.key = entry.key;
                Ok(())
            })?;
            scope
                .write_value_barrier(state, entry.key)
                .map_err(ExecutionError::HeapReference)?;
            scope
                .write_value_barrier(state, entry.value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }
}
