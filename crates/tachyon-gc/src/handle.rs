//! Typed, isolate-relative heap references.

use core::marker::PhantomData;

use tachyon_value::RawHeapRef;

/// A typed logical address into one isolate's GC span table.
///
/// This is intentionally only an encoded reference: resolving it into an object borrow belongs to
/// a future `RunningScope`/`NoGcScope` API. The phantom function preserves covariance without
/// making this four-byte representation inherit the pointee's auto-trait bounds.
#[derive(Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct GcRef<T: ?Sized> {
    raw: RawHeapRef,
    marker: PhantomData<fn() -> T>,
}

impl<T: ?Sized> Copy for GcRef<T> {}

impl<T: ?Sized> Clone for GcRef<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> GcRef<T> {
    /// Retypes a validated logical address at a descriptor-checked allocation or trace boundary.
    #[must_use]
    pub const fn from_raw(raw: RawHeapRef) -> Self {
        Self {
            raw,
            marker: PhantomData,
        }
    }

    /// Returns the encoded logical address without resolving it to a native pointer.
    #[must_use]
    pub const fn raw(self) -> RawHeapRef {
        self.raw
    }

    /// Erases the pointee type while retaining the isolate-relative identity.
    #[must_use]
    pub const fn erase(self) -> GcRef<()> {
        GcRef::from_raw(self.raw)
    }
}

const _: [(); 4] = [(); core::mem::size_of::<GcRef<()>>()];
const _: [(); 4] = [(); core::mem::align_of::<GcRef<()>>()];

#[cfg(test)]
mod tests {
    use super::GcRef;
    use tachyon_value::RawHeapRef;

    #[test]
    fn typed_reference_round_trips_without_changing_its_offset() {
        struct Object;

        let raw = RawHeapRef::new(16).expect("valid logical address");
        let reference = GcRef::<Object>::from_raw(raw);
        assert_eq!(reference.raw(), raw);
        assert_eq!(reference.erase().raw(), raw);
        assert_eq!(core::mem::size_of_val(&reference), 4);
    }
}
