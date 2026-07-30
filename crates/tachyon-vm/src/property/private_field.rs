//! Unforgeable private data slots backed by hidden shape keys.

use super::super::*;
use super::accessor::StoredProperty;

struct ProxyPrivateStorageRoots<'a> {
    vm: VmRoots<'a>,
    receiver: Value,
}

impl Trace for ProxyPrivateStorageRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.receiver.trace(tracer);
    }
}

impl Isolate {
    /// Converts only an engine-created Symbol payload into the hidden private-key domain.
    pub(crate) fn private_property_key(
        &mut self,
        value: Value,
    ) -> Result<PropertyKey, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::InvalidClassFieldPlan)?;
        let symbol = self
            .heap
            .checked_reference(raw, self.types.symbol)
            .map_err(|_| ExecutionError::InvalidClassFieldPlan)?;
        let serial = self.heap.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow_reference(symbol, self.types.symbol)
                .map(|symbol| symbol.serial)
                .map_err(ExecutionError::NoGcBorrow)
        })?;
        Ok(PropertyKey::Private(SymbolId::new(serial, raw)))
    }

    /// Adds a fresh private element after enforcing ordinary extensibility without Proxy traps.
    pub(crate) fn define_private_field(
        &mut self,
        receiver: Value,
        name: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.define_private_element(receiver, name, value, PropertyKind::Data, true)
    }

    /// Adds one immutable private method without exposing its storage as an ordinary descriptor.
    pub(crate) fn define_private_method(
        &mut self,
        receiver: Value,
        name: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.define_private_element(receiver, name, value, PropertyKind::Data, false)
    }

    /// Adds one shared getter/setter pair behind an unobservable private accessor slot.
    pub(crate) fn define_private_accessor(
        &mut self,
        receiver: Value,
        name: Value,
        pair: Value,
    ) -> Result<(), ExecutionError> {
        let raw = pair
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedAccessorDescriptor)?;
        self.heap
            .checked_reference(raw, self.types.accessor_pair)
            .map_err(ExecutionError::HeapReference)?;
        self.define_private_element(receiver, name, pair, PropertyKind::Accessor, false)
    }

    /// Shares private brand insertion while retaining the element kind's writability contract.
    fn define_private_element(
        &mut self,
        receiver: Value,
        name: Value,
        value: Value,
        kind: PropertyKind,
        writable: bool,
    ) -> Result<(), ExecutionError> {
        let key = self.private_property_key(name)?;
        let storage_receiver = self
            .proxy_private_storage(receiver, true)?
            .expect("private define always creates a Proxy sidecar");
        let (object, snapshot) = self.object_snapshot(storage_receiver)?;
        if !snapshot.extensible {
            return Err(ExecutionError::NonExtensibleObject(receiver));
        }
        if self.shapes.lookup(snapshot.shape, key).is_some() {
            return Err(ExecutionError::PrivateBrandCheckFailed(receiver));
        }
        self.add_property_slot_with_kind(
            object,
            snapshot,
            key,
            kind,
            value,
            PropertyAttributes::data(writable, false, false),
        )
    }

    /// Reads an own private field and never walks prototypes or invokes user code.
    pub(crate) fn get_private_field(
        &mut self,
        receiver: Value,
        name: Value,
    ) -> Result<PropertyRead, ExecutionError> {
        let key = self.private_property_key(name)?;
        let Some(storage_receiver) = self.proxy_private_storage(receiver, false)? else {
            return Err(ExecutionError::PrivateBrandCheckFailed(receiver));
        };
        let (_, snapshot) = self.object_snapshot(storage_receiver)?;
        let property = self
            .shapes
            .lookup(snapshot.shape, key)
            .ok_or(ExecutionError::PrivateBrandCheckFailed(receiver))?;
        match self.stored_property_from_snapshot(snapshot, property)? {
            Some(StoredProperty::Data(value)) => Ok(PropertyRead::Data(value)),
            Some(StoredProperty::Accessor { pair, .. }) => {
                if pair.getter.as_immediate() == Some(Immediate::Undefined) {
                    return Err(ExecutionError::ReadOnlyProperty(receiver));
                }
                Ok(PropertyRead::Accessor(pair.getter))
            }
            None => Err(ExecutionError::PrivateBrandCheckFailed(receiver)),
        }
    }

    /// Checks one own private element without invoking accessors, prototypes, or Proxy traps.
    pub(crate) fn has_private_element(
        &mut self,
        receiver: Value,
        name: Value,
    ) -> Result<bool, ExecutionError> {
        let key = self.private_property_key(name)?;
        let Some(storage_receiver) = self.proxy_private_storage(receiver, false)? else {
            return Ok(false);
        };
        let (_, snapshot) = self.object_snapshot(storage_receiver)?;
        Ok(self.shapes.lookup(snapshot.shape, key).is_some())
    }

    /// Writes an existing own private field while preserving its unobservable shape metadata.
    pub(crate) fn set_private_field(
        &mut self,
        receiver: Value,
        name: Value,
        value: Value,
    ) -> Result<PropertyWrite, ExecutionError> {
        let key = self.private_property_key(name)?;
        let Some(storage_receiver) = self.proxy_private_storage(receiver, false)? else {
            return Err(ExecutionError::PrivateBrandCheckFailed(receiver));
        };
        let (_, snapshot) = self.object_snapshot(storage_receiver)?;
        let property = self
            .shapes
            .lookup(snapshot.shape, key)
            .ok_or(ExecutionError::PrivateBrandCheckFailed(receiver))?;
        match self.stored_property_from_snapshot(snapshot, property)? {
            Some(StoredProperty::Data(_)) => {
                if !property.attributes.writable() {
                    return Err(ExecutionError::ReadOnlyProperty(receiver));
                }
                self.update_property_slot(snapshot, key, property.slot, value)?;
                Ok(PropertyWrite::Complete(true))
            }
            Some(StoredProperty::Accessor { pair, .. }) => {
                if pair.setter.as_immediate() == Some(Immediate::Undefined) {
                    return Err(ExecutionError::ReadOnlyProperty(receiver));
                }
                Ok(PropertyWrite::Setter(pair.setter))
            }
            None => Err(ExecutionError::PrivateBrandCheckFailed(receiver)),
        }
    }

    /// Returns the ordinary sidecar used for private slots on a Proxy, allocating it lazily.
    fn proxy_private_storage(
        &mut self,
        receiver: Value,
        allocate: bool,
    ) -> Result<Option<Value>, ExecutionError> {
        if !self.is_proxy_value(receiver) {
            return Ok(Some(receiver));
        }
        let raw = receiver
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(receiver))?;
        let proxy = self
            .heap
            .checked_reference(raw, self.types.proxy_object)
            .map_err(|_| ExecutionError::NotObject(receiver))?;
        if let Some(storage) = self.heap.with_running_scope(|scope| {
            let proxy = scope.root(proxy).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(proxy, self.types.proxy_object)
                    .map(|proxy| proxy.private_storage)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })? {
            return Ok(Some(Value::from_heap_ref(storage.raw())));
        }
        if !allocate {
            return Ok(None);
        }
        let mut roots = ProxyPrivateStorageRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                inactive_realms: &mut self.inactive_realms,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
            },
            receiver,
        };
        let storage = self
            .heap
            .try_allocate_with_gc(
                self.types.ordinary_object,
                0,
                0,
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype: Value::from_immediate(Immediate::Null),
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let receiver = roots.receiver;
        let proxy_raw = receiver
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(receiver))?;
        let proxy = self
            .heap
            .checked_reference(proxy_raw, self.types.proxy_object)
            .map_err(|_| ExecutionError::NotObject(receiver))?;
        self.heap.with_running_scope(|scope| {
            let proxy_local = scope.root(proxy).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(proxy_local, self.types.proxy_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .private_storage = Some(storage);
                Ok::<(), ExecutionError>(())
            })?;
            let storage_local = scope.root(storage).map_err(ExecutionError::Root)?;
            scope
                .write_barrier(proxy_local, storage_local)
                .map_err(ExecutionError::HeapReference)
        })?;
        Ok(Some(Value::from_heap_ref(storage.raw())))
    }
}
