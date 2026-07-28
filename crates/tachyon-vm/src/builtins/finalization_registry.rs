//! FinalizationRegistry construction, registration, and unregister-token matching.

use super::super::*;
use tachyon_gc::WeakGcRef;

impl Isolate {
    /// Constructs a registry after validating but never invoking its cleanup callback.
    pub(crate) fn create_finalization_registry_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_callable_value(callback)? {
            return Err(ExecutionError::NonCallable(callback));
        }
        let fallback = self
            .realm
            .finalization_registry_prototype
            .expect("FinalizationRegistry prototype initializes before construction");
        let prototype_atom = self.prototype_atom()?;
        let prototype = self
            .get_data_property(site.new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .unwrap_or(fallback);
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.finalization_registry_object,
                0,
                0,
                FinalizationRegistryObject {
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                    cleanup_callback: callback,
                    head: None,
                },
                AllocationSpace::Young,
                roots,
            )
            .map(|registry| Value::from_heap_ref(registry.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Appends one GC-managed registration cell without growing a Rust-side table.
    pub(crate) fn finalization_registry_register(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let registry = self.finalization_registry_reference(site.this_value)?;
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let target = self.weak_key(target)?;
        let held_value = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if self.same_value(target, held_value)? {
            return Err(ExecutionError::InvalidFinalizationRegistration(target));
        }
        let token_value = self
            .call_argument(site, 2)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let unregister_token = if token_value.as_immediate() == Some(Immediate::Undefined) {
            WeakGcRef::empty()
        } else {
            let token = self.weak_key(token_value)?;
            WeakGcRef::new(GcRef::from_erased_raw(
                token.as_heap_ref().expect("weak token was validated"),
            ))
        };
        let head = self.finalization_registry_snapshot(registry)?.head;
        let target = GcRef::from_erased_raw(target.as_heap_ref().expect("target was validated"));
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        let cell = self
            .heap
            .try_allocate_with_gc(
                self.types.finalization_cell,
                0,
                0,
                FinalizationCell {
                    registry,
                    registration: FinalizationRegistration::new(target, held_value),
                    unregister_token,
                    next: head,
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        self.set_finalization_registry_head(registry, cell)?;
        Ok(Value::from_immediate(Immediate::Undefined))
    }

    /// Deactivates every live cell whose weak unregister token has the supplied identity.
    pub(crate) fn finalization_registry_unregister(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let registry = self.finalization_registry_reference(site.this_value)?;
        let token = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let token = self.weak_key(token)?;
        let token = token.as_heap_ref().expect("weak token was validated");
        let mut cell = self.finalization_registry_snapshot(registry)?.head;
        let mut removed = false;
        while let Some(current) = cell {
            let (next, matches) = self.finalization_cell_snapshot(current, token)?;
            if matches {
                removed |= self.deactivate_finalization_cell(current)?;
            }
            cell = next;
        }
        Ok(Value::from_immediate(if removed {
            Immediate::True
        } else {
            Immediate::False
        }))
    }

    fn finalization_registry_reference(
        &self,
        receiver: Value,
    ) -> Result<GcRef<FinalizationRegistryObject>, ExecutionError> {
        receiver
            .as_heap_ref()
            .and_then(|raw| {
                self.heap
                    .checked_reference(raw, self.types.finalization_registry_object)
                    .ok()
            })
            .ok_or(ExecutionError::IncompatibleFinalizationRegistryReceiver(
                receiver,
            ))
    }

    fn finalization_registry_snapshot(
        &mut self,
        registry: GcRef<FinalizationRegistryObject>,
    ) -> Result<FinalizationRegistryObject, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let registry = scope.root(registry).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(registry, self.types.finalization_registry_object)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn set_finalization_registry_head(
        &mut self,
        registry: GcRef<FinalizationRegistryObject>,
        cell: GcRef<FinalizationCell>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let registry = scope.root(registry).map_err(ExecutionError::Root)?;
            let cell = scope.root(cell).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(registry, self.types.finalization_registry_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .head = Some(cell.as_gc_ref());
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_barrier(registry, cell)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    fn finalization_cell_snapshot(
        &mut self,
        cell: GcRef<FinalizationCell>,
        token: tachyon_value::RawHeapRef,
    ) -> Result<(Option<GcRef<FinalizationCell>>, bool), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let cell = scope.root(cell).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(cell, self.types.finalization_cell)
                    .map(|cell| {
                        (
                            cell.next,
                            cell.registration.target().is_some()
                                && cell
                                    .unregister_token
                                    .get()
                                    .is_some_and(|current| current.raw() == token),
                        )
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn deactivate_finalization_cell(
        &mut self,
        cell: GcRef<FinalizationCell>,
    ) -> Result<bool, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let cell = scope.root(cell).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(cell, self.types.finalization_cell)
                    .map(|cell| cell.registration.deactivate())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }
}
