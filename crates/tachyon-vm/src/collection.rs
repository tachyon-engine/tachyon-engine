//! Ordered collection backing shared by Map and Set exotics.

use core::mem::size_of;

use tachyon_gc::{GcExternalMemory, GcRef, Trace, Tracer};
use tachyon_value::Value;

use crate::object::OrdinaryObject;

/// Map exotic private slots plus its ordinary named-property base.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct MapObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) storage: GcRef<OrderedCollection>,
}

impl Trace for MapObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.storage.trace(tracer);
    }
}

/// Set exotic private slots plus its ordinary named-property base.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct SetObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) storage: GcRef<OrderedCollection>,
}

impl Trace for SetObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.storage.trace(tracer);
    }
}

/// One insertion-ordered entry retained as a tombstone after deletion for live iterators.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CollectionEntry {
    pub(crate) key: Value,
    pub(crate) value: Value,
}

impl Trace for CollectionEntry {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.key.trace(tracer);
        self.value.trace(tracer);
    }
}

/// Exact external backing for insertion-ordered ECMAScript collections.
///
/// Capacity is fixed at publication: growth must use a new GC-accounted payload rather than
/// changing an existing `GcExternalMemory` charge.
#[derive(Debug)]
pub(crate) struct OrderedCollection {
    entries: Box<[Option<CollectionEntry>]>,
    used: u32,
    live_len: u32,
}

impl OrderedCollection {
    /// Builds an exactly charged boxed backing with a checked allocation.
    pub(crate) fn with_capacity(capacity: usize) -> Result<Self, ()> {
        let mut entries = Vec::new();
        entries.try_reserve_exact(capacity).map_err(|_| ())?;
        entries.resize(capacity, None);
        Ok(Self {
            entries: entries.into_boxed_slice(),
            used: 0,
            live_len: 0,
        })
    }

    /// Returns one physical entry so callers can perform VM-level SameValueZero without holding a
    /// GC payload borrow across string comparison.
    #[inline(always)]
    pub(crate) fn entry_at(&self, index: u32) -> Option<CollectionEntry> {
        self.entries.get(index as usize).copied().flatten()
    }

    /// Replaces a known live entry value without changing insertion order or cardinality.
    pub(crate) fn update_at(&mut self, index: u32, value: Value) -> Result<(), ()> {
        let entry = self
            .entries
            .get_mut(index as usize)
            .and_then(Option::as_mut)
            .ok_or(())?;
        entry.value = value;
        Ok(())
    }

    /// Appends after the caller has checked capacity and canonicalized the key.
    pub(crate) fn append(&mut self, key: Value, value: Value) -> Result<(), ()> {
        let entry = self.entries.get_mut(self.used as usize).ok_or(())?;
        debug_assert!(entry.is_none());
        *entry = Some(CollectionEntry { key, value });
        self.used = self.used.checked_add(1).ok_or(())?;
        self.live_len = self.live_len.checked_add(1).ok_or(())?;
        Ok(())
    }

    /// Turns one known live position into a tombstone without shifting later cursor positions.
    pub(crate) fn delete_at(&mut self, index: u32) -> Result<(), ()> {
        let entry = self.entries.get_mut(index as usize).ok_or(())?;
        if entry.take().is_none() {
            return Err(());
        }
        self.live_len = self.live_len.saturating_sub(1);
        Ok(())
    }

    /// Clears live entries while retaining the historical cursor backing for existing iterators.
    pub(crate) fn clear(&mut self) {
        self.entries.fill(None);
        self.live_len = 0;
    }

    #[inline(always)]
    pub(crate) const fn len(&self) -> u32 {
        self.live_len
    }

    #[inline(always)]
    pub(crate) const fn used(&self) -> u32 {
        self.used
    }

    #[inline(always)]
    pub(crate) fn capacity(&self) -> usize {
        self.entries.len()
    }

    /// Copies physical positions, including tombstones, into a larger fixed backing.
    pub(crate) fn grow_copy(&self, capacity: usize) -> Result<Self, ()> {
        if capacity < self.used as usize {
            return Err(());
        }
        let mut grown = Self::with_capacity(capacity)?;
        grown.entries[..self.used as usize].copy_from_slice(&self.entries[..self.used as usize]);
        grown.used = self.used;
        grown.live_len = self.live_len;
        Ok(grown)
    }
}

impl Trace for OrderedCollection {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.entries.trace(tracer);
    }
}

impl GcExternalMemory for OrderedCollection {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.entries.len() * size_of::<Option<CollectionEntry>>()
    }
}

impl Default for OrderedCollection {
    fn default() -> Self {
        Self::with_capacity(0).expect("zero-capacity collection backing never allocates")
    }
}

#[cfg(test)]
mod tests {
    use super::OrderedCollection;
    use tachyon_value::Value;

    #[test]
    fn updates_and_deletes_keep_later_positions_stable() {
        let mut collection = OrderedCollection::with_capacity(2).unwrap();
        collection
            .append(Value::from_i32(1), Value::from_i32(10))
            .unwrap();
        collection
            .append(Value::from_i32(2), Value::from_i32(20))
            .unwrap();
        collection.delete_at(0).unwrap();
        assert_eq!(
            collection.entry_at(1).map(|entry| entry.value),
            Some(Value::from_i32(20)),
        );
        assert_eq!(collection.len(), 1);
    }
}
