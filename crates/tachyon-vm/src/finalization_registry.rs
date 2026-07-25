//! GC-managed FinalizationRegistry header and linked registration cells.

use tachyon_gc::{FinalizationRegistration, GcRef, Trace, Tracer, WeakGcRef};
use tachyon_value::Value;

use crate::object::OrdinaryObject;

/// Realm-owned cleanup callback and the head of its registration chain.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct FinalizationRegistryObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) cleanup_callback: Value,
    pub(crate) head: Option<GcRef<FinalizationCell>>,
}

impl Trace for FinalizationRegistryObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.cleanup_callback.trace(tracer);
        self.head.trace(tracer);
    }
}

/// One registration whose target and unregister token are weak edges.
#[derive(Debug)]
#[repr(C)]
pub(crate) struct FinalizationCell {
    pub(crate) registry: GcRef<FinalizationRegistryObject>,
    pub(crate) registration: FinalizationRegistration<()>,
    pub(crate) unregister_token: WeakGcRef<()>,
    pub(crate) next: Option<GcRef<FinalizationCell>>,
}

impl Trace for FinalizationCell {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.registry.trace(tracer);
        self.registration.trace(tracer);
        self.unregister_token.trace(tracer);
        self.next.trace(tracer);
    }
}

const _: [(); 40] = [(); core::mem::size_of::<FinalizationRegistryObject>()];
const _: [(); 32] = [(); core::mem::size_of::<FinalizationCell>()];
