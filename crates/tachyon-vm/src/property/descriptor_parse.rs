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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum PropertyDescriptorConsumer {
    Define,
    ReflectDefine,
    ProxyGetOwn(ProxyGetOwnMode),
    DefineProperties(Value),
}

/// GC-managed Object.defineProperties key scan retained across descriptor getters.
#[derive(Debug)]
pub(crate) struct PendingDefineProperties {
    target: Value,
    source: Value,
    keys: Box<[PropertyKey]>,
    index: usize,
    descriptors: Vec<PendingDefinedProperty>,
}

#[derive(Clone, Copy, Debug)]
struct PendingDefinedProperty {
    key: PropertyKey,
    descriptor: PropertyDescriptor,
}

impl Trace for PendingDefineProperties {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.target.trace(tracer);
        self.source.trace(tracer);
        self.keys.trace(tracer);
        for property in &mut self.descriptors {
            if let Some(symbol) = property.key.symbol() {
                let mut value = symbol.value();
                value.trace(tracer);
            }
            trace_property_descriptor(&mut property.descriptor, tracer);
        }
    }
}

impl GcExternalMemory for PendingDefineProperties {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.keys.len() * core::mem::size_of::<PropertyKey>()
            + self.descriptors.capacity() * core::mem::size_of::<PendingDefinedProperty>()
    }
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
    consumer: PropertyDescriptorConsumer,
}

impl PendingPropertyDescriptor {
    #[inline]
    fn new(
        target: Value,
        source: Value,
        key: PropertyKey,
        consumer: PropertyDescriptorConsumer,
    ) -> Self {
        Self {
            target,
            source,
            key,
            values: [Value::from_immediate(Immediate::Undefined); PROPERTY_DESCRIPTOR_FIELD_COUNT],
            present: 0,
            field: PropertyDescriptorField::FIRST,
            consumer,
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
        if let PropertyDescriptorConsumer::DefineProperties(state) = &mut self.consumer {
            state.trace(tracer);
        }
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
    /// Starts Object.defineProperties with an exact enumerable key snapshot.
    pub(crate) fn begin_object_define_properties(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let source = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_object_value(target) {
            return Err(ExecutionError::NotObject(target));
        }
        self.begin_define_properties_for_target(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            target,
            source,
        )
    }

    /// Starts the shared descriptor-map scan for an already validated object target.
    pub(crate) fn begin_define_properties_for_target(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        source: Value,
    ) -> Result<(), ExecutionError> {
        debug_assert!(self.is_object_value(target));
        if !self.is_object_value(source) {
            if matches!(
                source.as_immediate(),
                Some(Immediate::Undefined | Immediate::Null)
            ) {
                return Err(ExecutionError::NotObject(source));
            }
            if self.is_string_value(source) && self.string_value_length(source)? != 0 {
                return Err(ExecutionError::NotObject(source));
            }
            return self.write(site.caller_base, site.destination, target);
        }
        if self.is_proxy_value(source) {
            let state = self.allocate_define_properties_state(target, source, Vec::new())?;
            self.write(
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
            )?;
            return self
                .dispatch_define_properties_own_keys(site, state, source)
                .map(|_| ());
        }
        let (_, snapshot) = self.object_snapshot(source)?;
        let mut own_keys = self.ordinary_own_property_keys(source, snapshot)?;
        let mut keys = Vec::new();
        keys.try_reserve_exact(own_keys.len())
            .map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
        while let Some(entry) = own_keys.next_entry() {
            if entry
                .property
                .is_some_and(|property| property.attributes.enumerable())
            {
                keys.push(entry.key);
            }
        }
        let state = self.allocate_define_properties_state(target, source, keys)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.advance_define_properties(site, state)
    }

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
        reflect_result: bool,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(source) {
            return Err(ExecutionError::NotObject(source));
        }
        let consumer = if reflect_result {
            PropertyDescriptorConsumer::ReflectDefine
        } else {
            PropertyDescriptorConsumer::Define
        };
        let mut pending = PendingPropertyDescriptor::new(target, source, key, consumer);
        self.scan_property_descriptor(site, &mut pending)
    }

    /// Resumes the descriptor-map getter before scanning its six descriptor fields.
    pub(crate) fn resume_define_properties_descriptor_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingDefineProperties>,
        descriptor: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.pending_define_properties(state)?;
        let key = pending
            .key
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.begin_define_properties_descriptor(site, state, key, descriptor)
    }

    /// Resumes Proxy ownKeys, descriptor-enumerability, or Get for the descriptor map.
    pub(crate) fn resume_define_properties_stage(
        &mut self,
        continuation: NativeContinuation,
        stage: DefinePropertiesStage,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let site = continuation.site();
        let state = self.pending_define_properties_reference(continuation.first())?;
        match stage {
            DefinePropertiesStage::OwnKeys => {
                self.resume_define_properties_own_keys(site, state, value)
            }
            DefinePropertiesStage::Enumerable => {
                if !self.is_truthy_value(value)? {
                    self.advance_pending_define_properties(state)?;
                    return self.advance_define_properties(site, state).map(|_| None);
                }
                let key_value = continuation.second();
                let key = self.property_key(key_value)?;
                let source = self.pending_define_properties(state)?.source;
                self.dispatch_define_properties_get(site, state, source, key, key_value)
            }
            DefinePropertiesStage::Get => {
                let key = self.property_key(continuation.second())?;
                self.begin_define_properties_descriptor(site, state, key, value)
                    .map(|_| None)
            }
        }
    }

    /// Converts a materialized Proxy ownKeys array into an exact descriptor-map key list.
    fn resume_define_properties_own_keys(
        &mut self,
        site: NativeContinuationSite,
        old_state: GcRef<PendingDefineProperties>,
        result: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let length_key = PropertyKey::Atom(self.length_atom()?);
        let length = self
            .get_data_property(result, length_key)?
            .and_then(numeric_value)
            .ok_or(ExecutionError::ArrayLengthOverflow)? as usize;
        let mut keys = Vec::new();
        keys.try_reserve_exact(length)
            .map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
        for index in 0..length {
            let index_key = PropertyKey::Atom(self.safe_integer_property_atom(index as u64)?);
            let key = self
                .get_data_property(result, index_key)?
                .ok_or(ExecutionError::ProxyInvariantViolation)?;
            keys.push(self.property_key(key)?);
        }
        let pending = self.pending_define_properties(old_state)?;
        let state = self.allocate_define_properties_state(pending.target, pending.source, keys)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.advance_define_properties(site, state).map(|_| None)
    }

    /// Applies one parsed descriptor and continues the descriptor-map scan.
    fn finish_define_properties_descriptor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingDefineProperties>,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        self.push_pending_define_property(state, key, descriptor)?;
        self.advance_pending_define_properties(state)?;
        self.advance_define_properties(site, state)
    }

    /// Scans descriptor-map properties and suspends only on observable getters.
    fn advance_define_properties(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingDefineProperties>,
    ) -> Result<(), ExecutionError> {
        loop {
            let pending = self.pending_define_properties(state)?;
            let Some(key) = pending.key else {
                return self.apply_pending_define_properties(site, state);
            };
            if self.is_proxy_value(pending.source) {
                let key_value = self.define_properties_key_value(key)?;
                return self
                    .dispatch_define_properties_enumerable(site, state, pending.source, key_value)
                    .map(|_| ());
            }
            match self.resolve_property_read(pending.source, key)? {
                PropertyRead::Missing => self.advance_pending_define_properties(state)?,
                PropertyRead::Data(descriptor) => {
                    return self.begin_define_properties_descriptor(site, state, key, descriptor);
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    self.advance_pending_define_properties(state)?;
                }
                PropertyRead::Accessor(callee) => {
                    return self
                        .dispatch_property_callback(
                            NativeContinuation::array_iterator_property_get(
                                site,
                                PropertyCallbackMode::DefineProperties,
                                Value::from_heap_ref(state.raw()),
                                pending.source,
                            ),
                            callee,
                        )
                        .map(|_| ());
                }
            }
        }
    }

    fn define_properties_key_value(&mut self, key: PropertyKey) -> Result<Value, ExecutionError> {
        match key {
            PropertyKey::Atom(atom) => self.atom_string_value(atom),
            PropertyKey::Symbol(symbol) => Ok(symbol.value()),
            PropertyKey::Private(_) => Err(ExecutionError::PrivatePropertyKeyEscaped),
        }
    }

    fn dispatch_define_properties_own_keys(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingDefineProperties>,
        source: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let continuation = NativeContinuation::define_properties_stage(
            site,
            DefinePropertiesStage::OwnKeys,
            Value::from_heap_ref(state.raw()),
            Value::from_immediate(Immediate::Undefined),
        );
        self.dispatch_define_properties_proxy_operation(continuation, |isolate| {
            isolate.dispatch_proxy_own_keys(site, source, ProxyOwnKeysMode::Internal)
        })
    }

    fn dispatch_define_properties_enumerable(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingDefineProperties>,
        source: Value,
        key: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let continuation = NativeContinuation::define_properties_stage(
            site,
            DefinePropertiesStage::Enumerable,
            Value::from_heap_ref(state.raw()),
            key,
        );
        self.dispatch_define_properties_proxy_operation(continuation, |isolate| {
            isolate.dispatch_proxy_get_own(site, source, key, ProxyGetOwnMode::Enumerable)
        })
    }

    fn dispatch_define_properties_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingDefineProperties>,
        source: Value,
        key: PropertyKey,
        key_value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let continuation = NativeContinuation::define_properties_stage(
            site,
            DefinePropertiesStage::Get,
            Value::from_heap_ref(state.raw()),
            key_value,
        );
        self.dispatch_define_properties_proxy_operation(continuation, |isolate| {
            isolate.dispatch_proxy_aware_property_read(site, source, source, key)
        })
    }

    /// Runs one Proxy descriptor-map operation and drains a synchronous parent continuation.
    fn dispatch_define_properties_proxy_operation(
        &mut self,
        continuation: NativeContinuation,
        operation: impl FnOnce(&mut Self) -> Result<Option<RunOutcome>, ExecutionError>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let outcome = operation(self);
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() == completion_depth
        {
            return outcome;
        }
        let continuation = self.pop_native_continuation()?;
        let site = continuation.site();
        let value = self.read(site.caller_base, site.destination)?;
        let NativeContinuationKind::DefineProperties(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_define_properties_stage(continuation, stage, value)
    }

    fn begin_define_properties_descriptor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingDefineProperties>,
        key: PropertyKey,
        source: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(source) {
            return Err(ExecutionError::NotObject(source));
        }
        let mut pending = PendingPropertyDescriptor::new(
            self.pending_define_properties(state)?.target,
            source,
            key,
            PropertyDescriptorConsumer::DefineProperties(Value::from_heap_ref(state.raw())),
        );
        self.scan_property_descriptor(site, &mut pending)
    }

    /// Starts resumable ToPropertyDescriptor for one Proxy trap result.
    pub(crate) fn begin_proxy_property_descriptor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        mode: ProxyGetOwnMode,
        key: PropertyKey,
        source: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(source) {
            return Err(ExecutionError::ProxyInvariantViolation);
        }
        let mut pending = PendingPropertyDescriptor::new(
            Value::from_heap_ref(state.raw()),
            source,
            key,
            PropertyDescriptorConsumer::ProxyGetOwn(mode),
        );
        self.scan_property_descriptor(site, &mut pending)
    }

    /// Scans six descriptor fields in specification order and suspends only on accessor callbacks.
    fn scan_property_descriptor(
        &mut self,
        site: NativeContinuationSite,
        pending: &mut PendingPropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        loop {
            let field = pending.field;
            let atom = self.intern_intrinsic_name(field.name())?;
            match self.resolve_property_read(pending.source, atom.into())? {
                PropertyRead::Missing => {
                    let Some(next) = field.next() else {
                        return self.finish_pending_property_descriptor(site, *pending);
                    };
                    pending.field = next;
                }
                PropertyRead::Data(value) => {
                    pending.record(field, value);
                    if field.next().is_none() {
                        return self.finish_pending_property_descriptor(site, *pending);
                    }
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    pending.record(field, Value::from_immediate(Immediate::Undefined));
                    if field.next().is_none() {
                        return self.finish_pending_property_descriptor(site, *pending);
                    }
                }
                PropertyRead::Accessor(callee) => {
                    let state = self.allocate_pending_property_descriptor(*pending)?;
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
                promise_jobs: &mut self.promise_jobs,
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

    fn allocate_pending_define_properties(
        &mut self,
        pending: PendingDefineProperties,
    ) -> Result<GcRef<PendingDefineProperties>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_define_properties,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    fn allocate_define_properties_state(
        &mut self,
        target: Value,
        source: Value,
        keys: Vec<PropertyKey>,
    ) -> Result<GcRef<PendingDefineProperties>, ExecutionError> {
        let mut descriptors = Vec::new();
        descriptors
            .try_reserve_exact(keys.len())
            .map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
        self.allocate_pending_define_properties(PendingDefineProperties {
            target,
            source,
            keys: keys.into_boxed_slice(),
            index: 0,
            descriptors,
        })
    }

    pub(crate) fn pending_define_properties_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PendingDefineProperties>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_define_properties)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    fn pending_define_properties(
        &mut self,
        state: GcRef<PendingDefineProperties>,
    ) -> Result<PendingDefinePropertiesSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_define_properties)
                    .map(|pending| PendingDefinePropertiesSnapshot {
                        target: pending.target,
                        source: pending.source,
                        key: pending.keys.get(pending.index).copied(),
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn advance_pending_define_properties(
        &mut self,
        state: GcRef<PendingDefineProperties>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_define_properties)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending.index = pending.index.saturating_add(1);
                Ok(())
            })
        })
    }

    /// Appends one parsed descriptor without growing the pre-reserved vector.
    fn push_pending_define_property(
        &mut self,
        state: GcRef<PendingDefineProperties>,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_define_properties)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if pending.descriptors.len() == pending.descriptors.capacity() {
                    return Err(ExecutionError::OwnPropertyKeyAllocationFailed);
                }
                pending
                    .descriptors
                    .push(PendingDefinedProperty { key, descriptor });
                Ok(())
            })?;
            if let Some(symbol) = key.symbol() {
                scope
                    .write_value_barrier(state, symbol.value())
                    .map_err(ExecutionError::HeapReference)?;
            }
            for value in property_descriptor_edges(descriptor) {
                scope
                    .write_value_barrier(state, value)
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(())
        })
    }

    /// Applies the fully validated descriptor list only after every getter has succeeded.
    fn apply_pending_define_properties(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingDefineProperties>,
    ) -> Result<(), ExecutionError> {
        let (target, descriptors) = self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_define_properties)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let mut descriptors = Vec::new();
                descriptors
                    .try_reserve_exact(pending.descriptors.len())
                    .map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
                descriptors.extend_from_slice(&pending.descriptors);
                Ok((pending.target, descriptors))
            })
        })?;
        for property in descriptors {
            self.define_property(target, property.key, property.descriptor)?;
        }
        self.write(site.caller_base, site.destination, target)
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
        if let PropertyDescriptorConsumer::DefineProperties(state) = pending.consumer {
            let state = self.pending_define_properties_reference(state)?;
            return self.finish_define_properties_descriptor(site, state, pending.key, descriptor);
        }
        if let PropertyDescriptorConsumer::ProxyGetOwn(mode) = pending.consumer {
            let state = self.native_call_state_reference(pending.target)?;
            return self.finish_proxy_get_own_descriptor_parse(
                site,
                mode,
                state,
                descriptor.complete(),
            );
        }
        if self.is_proxy_value(pending.target) {
            let mode = if pending.consumer == PropertyDescriptorConsumer::ReflectDefine {
                ProxyDefineMode::Reflect
            } else {
                ProxyDefineMode::Object
            };
            return self
                .dispatch_proxy_define(site, pending.target, pending.key, descriptor, mode)
                .map(|_| ());
        }
        let defined = match self.define_property(pending.target, pending.key, descriptor) {
            Ok(()) => true,
            Err(
                ExecutionError::NonExtensibleObject(_)
                | ExecutionError::InvalidPropertyRedefinition(_),
            ) if pending.consumer == PropertyDescriptorConsumer::ReflectDefine => false,
            Err(error) => return Err(error),
        };
        let result = if pending.consumer == PropertyDescriptorConsumer::ReflectDefine {
            boolean_value(defined)
        } else {
            pending.target
        };
        self.write(site.caller_base, site.destination, result)
    }
}

#[derive(Clone, Copy)]
struct PendingDefinePropertiesSnapshot {
    target: Value,
    source: Value,
    key: Option<PropertyKey>,
}

fn trace_property_descriptor(descriptor: &mut PropertyDescriptor, tracer: &mut dyn Tracer) {
    match descriptor {
        PropertyDescriptor::Data(descriptor) => descriptor.value.trace(tracer),
        PropertyDescriptor::Accessor(descriptor) => {
            descriptor.getter.trace(tracer);
            descriptor.setter.trace(tracer);
        }
        PropertyDescriptor::Generic(_) => {}
    }
}

fn property_descriptor_edges(descriptor: PropertyDescriptor) -> impl Iterator<Item = Value> {
    let values = match descriptor {
        PropertyDescriptor::Data(descriptor) => [descriptor.value, None],
        PropertyDescriptor::Accessor(descriptor) => [descriptor.getter, descriptor.setter],
        PropertyDescriptor::Generic(_) => [None, None],
    };
    values.into_iter().flatten()
}
