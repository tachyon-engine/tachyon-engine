//! Accessor-pair allocation, checked slot recovery, and precise write barriers.

use super::super::*;

#[derive(Clone, Copy, Debug)]
pub(super) enum StoredProperty {
    Data(Value),
    Accessor {
        reference: GcRef<AccessorPair>,
        pair: AccessorPair,
    },
}

struct AccessorAllocationRoots<'a> {
    vm: VmRoots<'a>,
    receiver: Value,
    symbol_key: Option<Value>,
    getter: Value,
    setter: Value,
}

impl Trace for AccessorAllocationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.receiver.trace(tracer);
        self.symbol_key.trace(tracer);
        self.getter.trace(tracer);
        self.setter.trace(tracer);
    }
}

impl Isolate {
    /// Recovers a present data value or validates and copies one accessor-pair payload.
    pub(super) fn stored_property_from_snapshot(
        &mut self,
        snapshot: OrdinaryObject,
        property: PropertyLookup,
    ) -> Result<Option<StoredProperty>, ExecutionError> {
        let Some(value) = self.raw_property_value_from_snapshot(snapshot, property)? else {
            return Ok(None);
        };
        if property.kind == PropertyKind::Data {
            return Ok(Some(StoredProperty::Data(value)));
        }
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedAccessorDescriptor)?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.accessor_pair)
            .map_err(ExecutionError::HeapReference)?;
        let pair = self.heap.with_running_scope(|scope| {
            let local = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(local, self.types.accessor_pair)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        Ok(Some(StoredProperty::Accessor { reference, pair }))
    }

    /// Allocates a normalized pair while rooting every unpublished accessor edge.
    pub(super) fn allocate_accessor_pair(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        getter: Value,
        setter: Value,
    ) -> Result<(Value, Value), ExecutionError> {
        self.validate_accessor_callable(getter)?;
        self.validate_accessor_callable(setter)?;
        let mut roots = AccessorAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            receiver,
            symbol_key: key.symbol().map(SymbolId::value),
            getter,
            setter,
        };
        let pair = self
            .heap
            .try_allocate_with_gc(
                self.types.accessor_pair,
                0,
                0,
                AccessorPair {
                    getter: roots.getter,
                    setter: roots.setter,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        Ok((roots.receiver, Value::from_heap_ref(pair.raw())))
    }

    /// Applies a partial accessor update and remembers each new young callable from the pair owner.
    pub(super) fn update_accessor_pair(
        &mut self,
        reference: GcRef<AccessorPair>,
        getter: Option<Value>,
        setter: Option<Value>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let pair = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pair = no_gc
                    .borrow_mut(pair, self.types.accessor_pair)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if let Some(getter) = getter {
                    pair.getter = getter;
                }
                if let Some(setter) = setter {
                    pair.setter = setter;
                }
                Ok::<(), ExecutionError>(())
            })?;
            if let Some(getter) = getter {
                scope
                    .write_value_barrier(pair, getter)
                    .map_err(ExecutionError::HeapReference)?;
            }
            if let Some(setter) = setter {
                scope
                    .write_value_barrier(pair, setter)
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(())
        })
    }

    /// Accepts only callable objects or the normalized ECMAScript undefined sentinel.
    pub(super) fn validate_accessor_callable(
        &mut self,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if value.as_immediate() == Some(Immediate::Undefined) {
            return Ok(());
        }
        self.resolve_function_object(value).map(|_| ())
    }
}
