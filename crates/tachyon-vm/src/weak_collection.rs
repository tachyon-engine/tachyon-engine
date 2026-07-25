//! Ephemeron-backed private storage for WeakMap and WeakSet.

use core::mem::size_of;

use tachyon_gc::{Ephemeron, GcExternalMemory, GcRef, Trace, Tracer, WeakGcRef};
use tachyon_value::Value;

use crate::object::OrdinaryObject;

/// WeakMap exotic private slots plus its ordinary named-property base.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct WeakMapObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) storage: GcRef<WeakCollection>,
}

impl Trace for WeakMapObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.storage.trace(tracer);
    }
}

/// WeakSet exotic private slots plus its ordinary named-property base.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct WeakSetObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) storage: GcRef<WeakCollection>,
}

impl Trace for WeakSetObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.storage.trace(tracer);
    }
}

/// WeakRef private target plus its ordinary named-property base.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct WeakRefObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) target: WeakGcRef<()>,
}

impl Trace for WeakRefObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.target.trace(tracer);
    }
}

const _: [(); 32] = [(); core::mem::size_of::<WeakRefObject>()];

/// Fixed-capacity ephemeron table shared by one weak collection exotic.
///
/// A cleared ephemeron has no key and is reused by the next insertion. Capacity changes publish a
/// replacement payload so the external-memory charge remains exact.
#[derive(Debug)]
pub(crate) struct WeakCollection {
    entries: Box<[Option<Ephemeron<()>>]>,
}

impl WeakCollection {
    /// Creates an exactly charged ephemeron table with fallible backing allocation.
    pub(crate) fn with_capacity(capacity: usize) -> Result<Self, ()> {
        let mut entries = Vec::new();
        entries.try_reserve_exact(capacity).map_err(|_| ())?;
        entries.resize(capacity, None);
        Ok(Self {
            entries: entries.into_boxed_slice(),
        })
    }

    #[inline(always)]
    pub(crate) fn capacity(&self) -> usize {
        self.entries.len()
    }

    #[inline(always)]
    pub(crate) fn entry_at(&self, index: usize) -> Option<Ephemeron<()>> {
        self.entries.get(index).copied().flatten()
    }

    /// Publishes a new entry into an empty or collector-cleared slot.
    pub(crate) fn install_at(&mut self, index: usize, entry: Ephemeron<()>) -> Result<(), ()> {
        let slot = self.entries.get_mut(index).ok_or(())?;
        if slot.as_ref().is_some_and(|current| current.key().is_some()) {
            return Err(());
        }
        *slot = Some(entry);
        Ok(())
    }

    /// Replaces an existing ephemeron's conditional value without changing its weak key.
    pub(crate) fn update_at(&mut self, index: usize, value: Value) -> Result<(), ()> {
        self.entries
            .get_mut(index)
            .and_then(Option::as_mut)
            .ok_or(())?
            .replace_value(value);
        Ok(())
    }

    /// Clears one live key/value pair and makes its physical slot available for reuse.
    pub(crate) fn delete_at(&mut self, index: usize) -> Result<(), ()> {
        let slot = self.entries.get_mut(index).ok_or(())?;
        if slot.take().is_none() {
            return Err(());
        }
        Ok(())
    }

    /// Clones all physical slots into a larger exact-size backing.
    pub(crate) fn grow_copy(&self, capacity: usize) -> Result<Self, ()> {
        if capacity < self.capacity() {
            return Err(());
        }
        let mut grown = Self::with_capacity(capacity)?;
        grown.entries[..self.entries.len()].copy_from_slice(&self.entries);
        Ok(grown)
    }
}

impl Trace for WeakCollection {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.entries.trace(tracer);
    }
}

impl GcExternalMemory for WeakCollection {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.entries.len() * size_of::<Option<Ephemeron<()>>>()
    }
}

impl Default for WeakCollection {
    fn default() -> Self {
        Self::with_capacity(0).expect("zero-capacity weak collection never allocates")
    }
}
