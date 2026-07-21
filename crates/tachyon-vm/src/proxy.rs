//! Proxy identity, rooting, and ProxyCreate allocation substrate.

use super::*;

/// A Proxy has no ordinary-property base; every object internal method must use exotic dispatch.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct ProxyObject {
    pub(crate) target: Value,
    pub(crate) handler: Value,
}

impl Trace for ProxyObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.target.trace(tracer);
        self.handler.trace(tracer);
    }
}

struct ProxyAllocationRoots<'a> {
    vm: VmRoots<'a>,
    target: Value,
    handler: Value,
}

impl Trace for ProxyAllocationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.target.trace(tracer);
        self.handler.trace(tracer);
    }
}

impl Isolate {
    /// Validates ProxyCreate arguments before allocating the independently branded exotic payload.
    pub(crate) fn create_proxy_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let handler = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_object_value(target) {
            return Err(ExecutionError::NotObject(target));
        }
        if !self.is_object_value(handler) {
            return Err(ExecutionError::NotObject(handler));
        }
        let mut roots = ProxyAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            target,
            handler,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.proxy_object,
                0,
                0,
                ProxyObject {
                    target: roots.target,
                    handler: roots.handler,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|proxy| Value::from_heap_ref(proxy.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    #[inline(always)]
    pub(crate) fn is_proxy_value(&self, value: Value) -> bool {
        value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.proxy_object)
                .is_ok()
        })
    }

    /// Copies the two traced Proxy slots through a checked no-GC borrow.
    pub(crate) fn proxy_snapshot(&mut self, value: Value) -> Result<ProxyObject, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let proxy = self
            .heap
            .checked_reference(raw, self.types.proxy_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        self.heap.with_running_scope(|scope| {
            let proxy = scope.root(proxy).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(proxy, self.types.proxy_object)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Starts GetMethod(handler, trapName), suspending if the trap property is an accessor.
    pub(crate) fn dispatch_proxy_internal_method(
        &mut self,
        site: NativeContinuationSite,
        proxy: Value,
        operation: ProxyInternalMethod,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let snapshot = self.proxy_snapshot(proxy)?;
        if snapshot.handler.as_immediate() == Some(Immediate::Null) {
            return Err(ExecutionError::ProxyRevoked);
        }
        let trap_name = self.intern_intrinsic_name(operation.trap_name())?;
        match self.resolve_property_read(snapshot.handler, trap_name.into())? {
            PropertyRead::Missing => {
                self.forward_proxy_internal_method(site, proxy, snapshot.target, operation)
            }
            PropertyRead::Data(trap) => {
                self.continue_proxy_trap_lookup(site, proxy, operation, trap)
            }
            PropertyRead::Accessor(getter)
                if getter.as_immediate() == Some(Immediate::Undefined) =>
            {
                self.forward_proxy_internal_method(site, proxy, snapshot.target, operation)
            }
            PropertyRead::Accessor(getter) => self.dispatch_property_callback(
                NativeContinuation::proxy_trap_getter(site, operation, proxy, snapshot.handler),
                getter,
            ),
        }
    }

    /// Resumes either the trap-property getter or the actual trap call.
    pub(crate) fn resume_proxy_internal_method(
        &mut self,
        continuation: NativeContinuation,
        operation: ProxyInternalMethod,
        stage: ProxyContinuationStage,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match stage {
            ProxyContinuationStage::TrapGetter => self.continue_proxy_trap_lookup(
                continuation.site(),
                continuation.first(),
                operation,
                value,
            ),
            ProxyContinuationStage::TrapCall => self.finish_proxy_trap_call(
                continuation.site(),
                continuation.first(),
                operation,
                value,
            ),
        }
    }

    /// Applies GetMethod's nullish/callable rules and invokes one trap with the target argument.
    fn continue_proxy_trap_lookup(
        &mut self,
        site: NativeContinuationSite,
        proxy: Value,
        operation: ProxyInternalMethod,
        trap: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if matches!(
            trap.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            let target = self.proxy_snapshot(proxy)?.target;
            return self.forward_proxy_internal_method(site, proxy, target, operation);
        }
        self.resolve_function_object(trap)?;
        let target = self.proxy_snapshot(proxy)?.target;
        self.write(site.caller_base, site.destination, target)?;
        self.dispatch_property_callback(
            NativeContinuation::proxy_trap_call(site, operation, proxy, trap),
            trap,
        )
    }

    /// Executes the ordinary target operation used when the corresponding Proxy trap is absent.
    fn forward_proxy_internal_method(
        &mut self,
        site: NativeContinuationSite,
        proxy: Value,
        target: Value,
        operation: ProxyInternalMethod,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let (receiver, snapshot) = self.object_snapshot(target)?;
        let result = match operation {
            ProxyInternalMethod::GetPrototypeOf => snapshot.prototype,
            ProxyInternalMethod::IsExtensible => boolean_value(snapshot.extensible),
            ProxyInternalMethod::PreventExtensions => {
                self.set_object_extensible(receiver, false)?;
                boolean_value(true)
            }
            ProxyInternalMethod::PreventExtensionsObject => {
                self.set_object_extensible(receiver, false)?;
                proxy
            }
        };
        self.write(site.caller_base, site.destination, result)?;
        Ok(None)
    }

    /// Enforces the target invariant after a trap's normal completion.
    fn finish_proxy_trap_call(
        &mut self,
        site: NativeContinuationSite,
        proxy: Value,
        operation: ProxyInternalMethod,
        trap_result: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let target = self.proxy_snapshot(proxy)?.target;
        let (_, target_snapshot) = self.object_snapshot(target)?;
        let result = match operation {
            ProxyInternalMethod::GetPrototypeOf => {
                if trap_result.as_immediate() != Some(Immediate::Null)
                    && !self.is_object_value(trap_result)
                {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
                if !target_snapshot.extensible && trap_result != target_snapshot.prototype {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
                trap_result
            }
            ProxyInternalMethod::IsExtensible => {
                let result = self.is_truthy_value(trap_result)?;
                if result != target_snapshot.extensible {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
                boolean_value(result)
            }
            ProxyInternalMethod::PreventExtensions => {
                let result = self.is_truthy_value(trap_result)?;
                if result && target_snapshot.extensible {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
                boolean_value(result)
            }
            ProxyInternalMethod::PreventExtensionsObject => {
                let result = self.is_truthy_value(trap_result)?;
                if !result || target_snapshot.extensible {
                    return Err(ExecutionError::ProxyInvariantViolation);
                }
                proxy
            }
        };
        self.write(site.caller_base, site.destination, result)?;
        Ok(None)
    }
}

impl ProxyInternalMethod {
    #[inline(always)]
    const fn trap_name(self) -> &'static [u8] {
        match self {
            Self::GetPrototypeOf => b"getPrototypeOf",
            Self::IsExtensible => b"isExtensible",
            Self::PreventExtensions | Self::PreventExtensionsObject => b"preventExtensions",
        }
    }
}
