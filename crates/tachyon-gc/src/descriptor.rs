//! Static object descriptors used by the collector instead of per-object Rust trait objects.

use core::{alloc::Layout, ptr::NonNull};

use crate::{GcTypeId, Trace, Tracer};

/// The type-erased tracing entry point stored in a static descriptor.
///
/// # Safety
///
/// `object` must address a live, initialized instance of the concrete type registered by the same
/// descriptor. The collector is the sole caller and guarantees stop-the-world exclusivity.
pub type TraceObjectFn = unsafe fn(NonNull<u8>, &mut dyn Tracer);

/// The type-erased destruction entry point stored in a static descriptor.
///
/// # Safety
///
/// `object` must address a live, initialized instance of the concrete type registered by the same
/// descriptor, and this function must be called at most once for that allocation.
pub type DropObjectFn = unsafe fn(NonNull<u8>);

/// Immutable metadata for a concrete heap payload type.
#[derive(Clone, Copy)]
pub struct TypeDescriptor {
    type_id: GcTypeId,
    name: &'static str,
    layout: Layout,
    trace: TraceObjectFn,
    drop: DropObjectFn,
}

impl TypeDescriptor {
    /// Creates a descriptor whose callbacks are monomorphized for `T`.
    #[must_use]
    pub fn for_type<T: Trace>(type_id: GcTypeId, name: &'static str) -> Self {
        Self {
            type_id,
            name,
            layout: Layout::new::<T>(),
            trace: trace_object::<T>,
            drop: drop_object::<T>,
        }
    }

    /// Returns the static header type ID for this payload.
    #[must_use]
    pub const fn type_id(self) -> GcTypeId {
        self.type_id
    }

    /// Returns the diagnostic-only static type name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Returns the payload layout; the allocator accounts for the separate `GcHeader` prefix.
    #[must_use]
    pub const fn layout(self) -> Layout {
        self.layout
    }

    /// Invokes the concrete tracing callback through the checked descriptor boundary.
    ///
    /// # Safety
    ///
    /// The caller must uphold [`TraceObjectFn`] for the descriptor returned by [`Self::for_type`].
    pub unsafe fn trace(self, object: NonNull<u8>, tracer: &mut dyn Tracer) {
        // SAFETY: The caller establishes that `object` has this descriptor's registered concrete type.
        unsafe { (self.trace)(object, tracer) };
    }

    /// Invokes the concrete destructor through the checked descriptor boundary.
    ///
    /// # Safety
    ///
    /// The caller must uphold [`DropObjectFn`] for the descriptor returned by [`Self::for_type`].
    pub unsafe fn drop(self, object: NonNull<u8>) {
        // SAFETY: The caller establishes unique ownership of the live object and invokes this once.
        unsafe { (self.drop)(object) };
    }
}

/// Casts the collector-provided allocation back to its descriptor-registered payload type.
///
/// This is the sole raw-pointer dereference in the descriptor layer. `TypeDescriptor::for_type`
/// pairs this monomorphization with its layout, while the allocator will validate the logical span,
/// alignment, allocation bit, and header type ID before invoking it.
unsafe fn trace_object<T: Trace>(object: NonNull<u8>, tracer: &mut dyn Tracer) {
    // SAFETY: `TypeDescriptor::trace` requires a live initialized `T` at this exact address.
    unsafe { &mut *object.cast::<T>().as_ptr() }.trace(tracer);
}

/// Drops one descriptor-registered payload after sweep has removed its allocation metadata.
unsafe fn drop_object<T>(object: NonNull<u8>) {
    // SAFETY: `TypeDescriptor::drop` requires exclusive ownership of one live `T` allocation.
    unsafe { core::ptr::drop_in_place(object.cast::<T>().as_ptr()) };
}

#[cfg(test)]
mod tests {
    use core::{mem::ManuallyDrop, ptr::NonNull};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tachyon_value::{RawHeapRef, Value};

    use super::TypeDescriptor;
    use crate::{GcRef, GcTypeId, Trace, Tracer};

    #[derive(Default)]
    struct CountingTracer(usize);

    impl Tracer for CountingTracer {
        fn trace_value(&mut self, value: &mut Value) {
            self.0 += usize::from(value.as_heap_ref().is_some());
        }

        fn trace_raw_heap_ref(&mut self, _: &mut RawHeapRef) {
            self.0 += 1;
        }
    }

    struct Pair {
        left: GcRef<()>,
        right: Value,
    }

    impl Trace for Pair {
        fn trace(&mut self, tracer: &mut dyn Tracer) {
            self.left.trace(tracer);
            self.right.trace(tracer);
        }
    }

    #[test]
    fn descriptor_traces_its_registered_payload_type() {
        let raw = RawHeapRef::new(16).expect("valid logical address");
        let mut pair = Pair {
            left: GcRef::from_raw(raw),
            right: Value::from_heap_ref(raw),
        };
        let descriptor = TypeDescriptor::for_type::<Pair>(
            GcTypeId::new(1).expect("non-zero descriptor ID"),
            "Pair",
        );
        let mut tracer = CountingTracer::default();

        // SAFETY: `pair` is an initialized `Pair` with the descriptor created for `Pair`.
        unsafe { descriptor.trace(NonNull::from(&mut pair).cast(), &mut tracer) };

        assert_eq!(descriptor.name(), "Pair");
        assert_eq!(descriptor.type_id().index(), 1);
        assert_eq!(descriptor.layout(), core::alloc::Layout::new::<Pair>());
        assert_eq!(tracer.0, 2);
    }

    static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

    struct DropProbe;

    impl Trace for DropProbe {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            DROP_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn descriptor_drops_its_registered_payload_exactly_once() {
        DROP_COUNT.store(0, Ordering::Relaxed);
        let descriptor = TypeDescriptor::for_type::<DropProbe>(
            GcTypeId::new(2).expect("non-zero descriptor ID"),
            "DropProbe",
        );
        let raw = Box::into_raw(Box::new(DropProbe));

        // SAFETY: `raw` originates from a live `Box<DropProbe>` and has not been dropped.
        unsafe { descriptor.drop(NonNull::new(raw).expect("Box is never null").cast()) };
        // SAFETY: The descriptor has dropped the payload; `ManuallyDrop` frees the allocation only.
        unsafe { drop(Box::from_raw(raw.cast::<ManuallyDrop<DropProbe>>())) };

        assert_eq!(DROP_COUNT.load(Ordering::Relaxed), 1);
    }
}
