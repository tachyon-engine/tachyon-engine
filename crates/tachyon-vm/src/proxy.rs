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
}
