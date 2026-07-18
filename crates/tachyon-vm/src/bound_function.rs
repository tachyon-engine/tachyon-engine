//! GC-managed bound-function slots and exact immutable argument backing.

use core::mem::size_of;

use tachyon_gc::{GcExternalMemory, Trace, Tracer};
use tachyon_value::Value;

/// Immutable bound-function exotic slots shared by call and construct forwarding.
#[derive(Debug)]
pub(crate) struct BoundFunctionData {
    pub(crate) bound_target: Value,
    pub(crate) call_target: Value,
    pub(crate) bound_this: Value,
    pub(crate) arguments: Box<[Value]>,
    pub(crate) length: Value,
    pub(crate) name: Value,
}

impl Trace for BoundFunctionData {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.bound_target.trace(tracer);
        self.call_target.trace(tracer);
        self.bound_this.trace(tracer);
        self.arguments.trace(tracer);
        self.name.trace(tracer);
    }
}

impl GcExternalMemory for BoundFunctionData {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.arguments.len() * size_of::<Value>()
    }
}
