//! The exact, rewrite-capable object graph traversal contract.

use tachyon_value::{RawHeapRef, Value};

use crate::GcRef;

/// Visits every GC edge owned by an object or root.
///
/// References are mutable from the first collector phase so every collector uses one exact visitor
/// contract. Tachyon 1.0 does not relocate objects; mutability does not imply a moving nursery.
pub trait Trace {
    /// Traces every direct GC edge held by `self`.
    fn trace(&mut self, tracer: &mut dyn Tracer);
}

/// Receives exact heap references during graph traversal.
///
/// Implementations may rewrite an internal reference encoding before the call returns, while the
/// 1.0 collector keeps every object's logical address and native address stable.
pub trait Tracer {
    /// Visits a NaN-boxed JavaScript value, including a potential heap-reference payload.
    fn trace_value(&mut self, value: &mut Value);

    /// Visits an encoded object reference held outside a `Value`.
    fn trace_raw_heap_ref(&mut self, reference: &mut RawHeapRef);
}

impl Trace for Value {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        tracer.trace_value(self);
    }
}

impl Trace for RawHeapRef {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        tracer.trace_raw_heap_ref(self);
    }
}

impl<T: ?Sized> Trace for GcRef<T> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        let mut raw = self.raw();
        tracer.trace_raw_heap_ref(&mut raw);
        *self = Self::from_raw(raw);
    }
}

impl<T: Trace> Trace for Option<T> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        if let Some(value) = self {
            value.trace(tracer);
        }
    }
}

impl<T: Trace> Trace for [T] {
    /// Traces a contiguous field collection without allocating a traversal work item per element.
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        for value in self {
            value.trace(tracer);
        }
    }
}

impl<T: Trace> Trace for Vec<T> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.as_mut_slice().trace(tracer);
    }
}

#[cfg(test)]
mod tests {
    use tachyon_value::{RawHeapRef, Value};

    use super::{Trace, Tracer};
    use crate::GcRef;

    #[derive(Default)]
    struct RewritingTracer {
        values: usize,
        raw_references: usize,
    }

    impl Tracer for RewritingTracer {
        fn trace_value(&mut self, value: &mut Value) {
            self.values += 1;
            if let Some(reference) = value.as_heap_ref() {
                *value = Value::from_heap_ref(rewrite(reference));
            }
        }

        fn trace_raw_heap_ref(&mut self, reference: &mut RawHeapRef) {
            self.raw_references += 1;
            *reference = rewrite(*reference);
        }
    }

    fn rewrite(reference: RawHeapRef) -> RawHeapRef {
        RawHeapRef::new(reference.offset() + 16).expect("test offsets stay non-zero")
    }

    #[test]
    fn trace_rewrites_values_and_typed_references() {
        struct Object;

        let raw = RawHeapRef::new(16).expect("valid logical address");
        let mut fields = [
            Value::from_heap_ref(raw),
            Value::from_i32(1),
            Value::from_heap_ref(raw),
        ];
        let mut reference = GcRef::<Object>::from_raw(raw);
        let mut tracer = RewritingTracer::default();

        fields.trace(&mut tracer);
        reference.trace(&mut tracer);

        assert_eq!(tracer.values, 3);
        assert_eq!(tracer.raw_references, 1);
        assert_eq!(fields[0].as_heap_ref(), RawHeapRef::new(32));
        assert_eq!(fields[1].as_i32(), Some(1));
        assert_eq!(fields[2].as_heap_ref(), RawHeapRef::new(32));
        assert_eq!(
            reference.raw(),
            RawHeapRef::new(32).expect("valid logical address")
        );
    }
}
