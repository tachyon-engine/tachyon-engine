//! Resumable ToPropertyDescriptor field reads and their GC-managed callback state.

use super::super::*;

const PROPERTY_DESCRIPTOR_FIELD_COUNT: usize = 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum PropertyDescriptorField {
    Enumerable,
    Configurable,
    Value,
    Writable,
    Get,
    Set,
}

impl PropertyDescriptorField {
    const FIRST: Self = Self::Enumerable;

    #[inline]
    const fn index(self) -> usize {
        self as usize
    }

    #[inline]
    const fn next(self) -> Option<Self> {
        match self {
            Self::Enumerable => Some(Self::Configurable),
            Self::Configurable => Some(Self::Value),
            Self::Value => Some(Self::Writable),
            Self::Writable => Some(Self::Get),
            Self::Get => Some(Self::Set),
            Self::Set => None,
        }
    }

    #[inline]
    const fn name(self) -> &'static [u8] {
        match self {
            Self::Enumerable => b"enumerable",
            Self::Configurable => b"configurable",
            Self::Value => b"value",
            Self::Writable => b"writable",
            Self::Get => b"get",
            Self::Set => b"set",
        }
    }
}

/// GC-managed partial ToPropertyDescriptor state retained only after the first getter suspension.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingPropertyDescriptor {
    target: Value,
    source: Value,
    key: PropertyKey,
    values: [Value; PROPERTY_DESCRIPTOR_FIELD_COUNT],
    present: u8,
    field: PropertyDescriptorField,
}

impl PendingPropertyDescriptor {
    #[inline]
    fn new(target: Value, source: Value, key: PropertyKey) -> Self {
        Self {
            target,
            source,
            key,
            values: [Value::from_immediate(Immediate::Undefined); PROPERTY_DESCRIPTOR_FIELD_COUNT],
            present: 0,
            field: PropertyDescriptorField::FIRST,
        }
    }

    #[inline]
    fn value(self, field: PropertyDescriptorField) -> Option<Value> {
        (self.present & (1 << field.index()) != 0).then_some(self.values[field.index()])
    }

    #[inline]
    fn record(&mut self, field: PropertyDescriptorField, value: Value) {
        self.values[field.index()] = value;
        self.present |= 1 << field.index();
        if let Some(next) = field.next() {
            self.field = next;
        }
    }

    /// Closes all presence-aware fields without conflating absent with present undefined.
    fn finish(self, isolate: &mut Isolate) -> Result<PropertyDescriptor, ExecutionError> {
        let enumerable = self
            .value(PropertyDescriptorField::Enumerable)
            .map(|value| isolate.is_truthy_value(value))
            .transpose()?;
        let configurable = self
            .value(PropertyDescriptorField::Configurable)
            .map(|value| isolate.is_truthy_value(value))
            .transpose()?;
        let value = self.value(PropertyDescriptorField::Value);
        let writable = self
            .value(PropertyDescriptorField::Writable)
            .map(|value| isolate.is_truthy_value(value))
            .transpose()?;
        let getter = self.value(PropertyDescriptorField::Get);
        let setter = self.value(PropertyDescriptorField::Set);
        if getter.is_some() || setter.is_some() {
            if value.is_some() || writable.is_some() {
                return Err(ExecutionError::InvalidPropertyDescriptor(self.source));
            }
            if let Some(getter) = getter {
                isolate.validate_accessor_callable(getter)?;
            }
            if let Some(setter) = setter {
                isolate.validate_accessor_callable(setter)?;
            }
            return Ok(PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter,
                setter,
                enumerable,
                configurable,
            }));
        }
        if value.is_some() || writable.is_some() {
            return Ok(PropertyDescriptor::Data(DataPropertyDescriptor {
                value,
                writable,
                enumerable,
                configurable,
            }));
        }
        Ok(PropertyDescriptor::Generic(GenericPropertyDescriptor {
            enumerable,
            configurable,
        }))
    }
}

impl Trace for PendingPropertyDescriptor {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.target.trace(tracer);
        self.source.trace(tracer);
        if let Some(symbol) = self.key.symbol() {
            let mut value = symbol.value();
            value.trace(tracer);
        }
        self.values.trace(tracer);
    }
}

struct PendingPropertyDescriptorRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingPropertyDescriptor,
}

impl Trace for PendingPropertyDescriptorRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    #[inline]
    pub(crate) fn pending_property_descriptor_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PendingPropertyDescriptor>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_property_descriptor)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    #[inline]
    pub(crate) fn pending_property_descriptor_source(
        &mut self,
        state: GcRef<PendingPropertyDescriptor>,
    ) -> Result<Value, ExecutionError> {
        self.pending_property_descriptor(state)
            .map(|pending| pending.source)
    }

    /// Starts ToPropertyDescriptor on the allocation-free data path and suspends only for getters.
    pub(crate) fn begin_property_descriptor(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        key: PropertyKey,
        source: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(source) {
            return Err(ExecutionError::NotObject(source));
        }
        let mut pending = PendingPropertyDescriptor::new(target, source, key);
        loop {
            let field = pending.field;
            let atom = self.intern_intrinsic_name(field.name())?;
            match self.resolve_property_read(source, atom.into())? {
                PropertyRead::Missing => {
                    let Some(next) = field.next() else {
                        return self.finish_pending_property_descriptor(site, pending);
                    };
                    pending.field = next;
                }
                PropertyRead::Data(value) => {
                    pending.record(field, value);
                    if field.next().is_none() {
                        return self.finish_pending_property_descriptor(site, pending);
                    }
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    pending.record(field, Value::from_immediate(Immediate::Undefined));
                    if field.next().is_none() {
                        return self.finish_pending_property_descriptor(site, pending);
                    }
                }
                PropertyRead::Accessor(callee) => {
                    let state = self.allocate_pending_property_descriptor(pending)?;
                    self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_heap_ref(state.raw()),
                    )?;
                    return self.call_property_descriptor_callback(site, state, callee);
                }
            }
        }
    }

    /// Records one getter result and resumes the fixed-order descriptor scan from managed state.
    pub(crate) fn resume_property_descriptor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingPropertyDescriptor>,
        returned: Value,
    ) -> Result<(), ExecutionError> {
        let returned_field = self.pending_property_descriptor(state)?.field;
        self.update_pending_property_descriptor(state, returned)?;
        if returned_field.next().is_none() {
            let pending = self.pending_property_descriptor(state)?;
            return self.finish_pending_property_descriptor(site, pending);
        }
        loop {
            let pending = self.pending_property_descriptor(state)?;
            let field = pending.field;
            let atom = self.intern_intrinsic_name(field.name())?;
            match self.resolve_property_read(pending.source, atom.into())? {
                PropertyRead::Missing => {
                    if let Some(next) = field.next() {
                        self.advance_pending_property_descriptor(state, next)?;
                    } else {
                        return self.finish_pending_property_descriptor(site, pending);
                    }
                }
                PropertyRead::Data(value) => {
                    self.update_pending_property_descriptor(state, value)?;
                    if field.next().is_none() {
                        let pending = self.pending_property_descriptor(state)?;
                        return self.finish_pending_property_descriptor(site, pending);
                    }
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    self.update_pending_property_descriptor(
                        state,
                        Value::from_immediate(Immediate::Undefined),
                    )?;
                    if field.next().is_none() {
                        let pending = self.pending_property_descriptor(state)?;
                        return self.finish_pending_property_descriptor(site, pending);
                    }
                }
                PropertyRead::Accessor(callee) => {
                    return self.call_property_descriptor_callback(site, state, callee);
                }
            }
        }
    }

    /// Allocates cold partial state with every unpublished edge visible to a forced collection.
    fn allocate_pending_property_descriptor(
        &mut self,
        pending: PendingPropertyDescriptor,
    ) -> Result<GcRef<PendingPropertyDescriptor>, ExecutionError> {
        let mut roots = PendingPropertyDescriptorRoots {
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
                self.types.pending_property_descriptor,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Copies managed state without retaining a heap borrow across lookup or allocation.
    fn pending_property_descriptor(
        &mut self,
        state: GcRef<PendingPropertyDescriptor>,
    ) -> Result<PendingPropertyDescriptor, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_property_descriptor)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Publishes one returned getter value and its barrier before the next observable Get.
    fn update_pending_property_descriptor(
        &mut self,
        state: GcRef<PendingPropertyDescriptor>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_property_descriptor)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending.record(pending.field, value);
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map(|_| ())
                .map_err(ExecutionError::HeapReference)
        })
    }

    /// Skips one absent field while retaining every previously observed descriptor value.
    fn advance_pending_property_descriptor(
        &mut self,
        state: GcRef<PendingPropertyDescriptor>,
        next: PropertyDescriptorField,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.pending_property_descriptor)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .field = next;
                Ok(())
            })
        })
    }

    /// Validates the closed descriptor, applies it, and replaces the temporary destination root.
    fn finish_pending_property_descriptor(
        &mut self,
        site: NativeContinuationSite,
        pending: PendingPropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        let descriptor = pending.finish(self)?;
        self.define_property(pending.target, pending.key, descriptor)?;
        self.write(site.caller_base, site.destination, pending.target)
    }
}
