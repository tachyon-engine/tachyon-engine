//! Resumable ToPropertyKey consumers for Object builtins.

use super::super::*;

/// Two builtin operands retained only while an object key executes JavaScript conversion code.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingNativePropertyKey {
    first: Value,
    second: Value,
}

impl PendingNativePropertyKey {
    #[inline]
    pub(crate) const fn new(first: Value, second: Value) -> Self {
        Self { first, second }
    }
}

impl Trace for PendingNativePropertyKey {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.first.trace(tracer);
        self.second.trace(tracer);
    }
}

const _: [(); 16] = [(); core::mem::size_of::<PendingNativePropertyKey>()];

struct PendingNativePropertyKeyRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingNativePropertyKey,
    key: Value,
}

impl Trace for PendingNativePropertyKeyRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
        self.key.trace(tracer);
    }
}

impl Isolate {
    /// Converts one builtin key, allocating pending state only when the key can call JavaScript.
    pub(crate) fn dispatch_builtin_property_key(
        &mut self,
        consumer: BuiltinPropertyKeyConsumer,
        site: &CallSite,
        first: Value,
        key: Value,
        second: Value,
    ) -> Result<(), ExecutionError> {
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let pending = PendingNativePropertyKey::new(first, second);
        if !self.is_object_value(key) {
            return self.finish_builtin_property_key(native_site, consumer, pending, key);
        }
        let state = self.allocate_pending_native_property_key(pending, key)?;
        self.dispatch_object_primitive_conversion(
            ConversionConsumer::BuiltinPropertyKey(consumer),
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
            key,
            site.call_site,
        )
    }

    /// Applies one converted key to the exact builtin operation and writes its completed result.
    pub(crate) fn finish_builtin_property_key(
        &mut self,
        site: NativeContinuationSite,
        consumer: BuiltinPropertyKeyConsumer,
        pending: PendingNativePropertyKey,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        self.write(site.caller_base, site.destination, primitive)?;
        let key = self.property_key(primitive)?;
        match consumer {
            BuiltinPropertyKeyConsumer::DefineProperty => {
                self.begin_property_descriptor(site, pending.first, key, pending.second)
            }
            BuiltinPropertyKeyConsumer::GetOwnPropertyDescriptor => {
                self.finish_get_own_property_descriptor(site, pending.first, key)
            }
            BuiltinPropertyKeyConsumer::HasOwnProperty => {
                self.finish_builtin_has_own(site, pending.first, key)
            }
            BuiltinPropertyKeyConsumer::PropertyIsEnumerable => {
                self.finish_property_is_enumerable(site, pending.first, key)
            }
            BuiltinPropertyKeyConsumer::HasOwn => {
                self.finish_builtin_has_own(site, pending.first, key)
            }
        }
    }

    /// Materializes a descriptor only after key conversion has completed successfully.
    fn finish_get_own_property_descriptor(
        &mut self,
        site: NativeContinuationSite,
        object: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let property = if self.is_object_value(object) {
            self.complete_own_property_descriptor(object, key)?
        } else {
            None
        };
        let Some(descriptor) = property else {
            return self.write(
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::Undefined),
            );
        };
        let result = self.create_ordinary_object()?;
        self.write(site.caller_base, site.destination, result)?;
        self.materialize_property_descriptor(result, descriptor)
    }

    /// Completes static or prototype own-property queries with their distinct nullish ordering.
    fn finish_builtin_has_own(
        &mut self,
        site: NativeContinuationSite,
        object: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        if is_nullish(object) {
            return Err(ExecutionError::NotObject(object));
        }
        let result = if self.is_object_value(object) {
            self.has_own_property(object, key)?
        } else {
            false
        };
        self.write(site.caller_base, site.destination, boolean_value(result))
    }

    /// Completes propertyIsEnumerable after its required key-first conversion order.
    fn finish_property_is_enumerable(
        &mut self,
        site: NativeContinuationSite,
        object: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        if is_nullish(object) {
            return Err(ExecutionError::NotObject(object));
        }
        let enumerable = if self.is_object_value(object) {
            self.complete_own_property_descriptor(object, key)?
                .is_some_and(|descriptor| descriptor.enumerable().unwrap_or(false))
        } else {
            false
        };
        self.write(
            site.caller_base,
            site.destination,
            boolean_value(enumerable),
        )
    }

    /// Allocates and roots both pending operands plus the not-yet-published object key.
    fn allocate_pending_native_property_key(
        &mut self,
        pending: PendingNativePropertyKey,
        key: Value,
    ) -> Result<GcRef<PendingNativePropertyKey>, ExecutionError> {
        let mut roots = PendingNativePropertyKeyRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            pending,
            key,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_native_property_key,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Restores one validated pending payload without retaining a borrow across builtin work.
    pub(crate) fn pending_native_property_key(
        &mut self,
        value: Value,
    ) -> Result<PendingNativePropertyKey, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        let state = self
            .heap
            .checked_reference(raw, self.types.pending_native_property_key)
            .map_err(|_| ExecutionError::MissingNativeContinuation)?;
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_native_property_key)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }
}
