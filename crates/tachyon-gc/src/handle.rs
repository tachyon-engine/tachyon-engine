//! Typed, isolate-relative heap references.

use core::{
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
};

use tachyon_value::RawHeapRef;

/// A typed logical address into one isolate's GC span table.
///
/// This is intentionally only an encoded reference: resolving it into an object borrow belongs to
/// a future `RunningScope`/`NoGcScope` API. The phantom function preserves covariance without
/// making this four-byte representation inherit the pointee's auto-trait bounds.
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

impl<T: ?Sized> fmt::Debug for GcRef<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("GcRef").field(&self.raw).finish()
    }
}

impl<T: ?Sized> PartialEq for GcRef<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<T: ?Sized> Eq for GcRef<T> {}

impl<T: ?Sized> Hash for GcRef<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl<T: ?Sized> GcRef<T> {
    /// Retypes a validated logical address at a descriptor-checked allocation or trace boundary.
    #[must_use]
    pub(crate) const fn from_raw(raw: RawHeapRef) -> Self {
        Self {
            raw,
            marker: PhantomData,
        }
    }

    /// Retypes an encoded address at an external low-level boundary.
    ///
    /// # Safety
    ///
    /// Before this reference can be dereferenced, the caller must validate that it belongs to the
    /// target heap and that its registered descriptor is exactly `T`. Prefer allocator-produced
    /// `GcRef<T>` or `RunningScope` APIs whenever possible.
    #[must_use]
    pub const unsafe fn from_raw_unchecked(raw: RawHeapRef) -> Self {
        Self::from_raw(raw)
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
