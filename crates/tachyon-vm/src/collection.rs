//! Ordered collection backing shared by Map and Set exotics.

use core::mem::size_of;

use tachyon_gc::{GcExternalMemory, Trace, Tracer};
use tachyon_value::Value;

/// One insertion-ordered entry retained as a tombstone after deletion for live iterators.
#[derive(Clone, Copy, Debug)]
#[allow(
    dead_code,
    reason = "Map and Set object payloads consume this in the next collection slice"
)]
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
#[derive(Debug, Default)]
#[allow(
    dead_code,
    reason = "Map and Set object payloads consume this in the next collection slice"
)]
pub(crate) struct OrderedCollection {
    entries: Vec<Option<CollectionEntry>>,
    live_len: u32,
}

#[allow(
    dead_code,
    reason = "Map and Set native methods consume this in the next collection slice"
)]
impl OrderedCollection {
    /// Updates an existing entry or appends one new insertion while preserving cursor stability.
    pub(crate) fn insert_or_update(
        &mut self,
        key: Value,
        value: Value,
        mut equal: impl FnMut(Value, Value) -> bool,
    ) -> Result<bool, ()> {
        for entry in self.entries.iter_mut().flatten() {
            if equal(entry.key, key) {
                entry.value = value;
                return Ok(false);
            }
        }
        self.entries.try_reserve(1).map_err(|_| ())?;
        self.entries.push(Some(CollectionEntry { key, value }));
        self.live_len = self.live_len.checked_add(1).ok_or(())?;
        Ok(true)
    }

    /// Marks a matching entry deleted without shifting later iterator positions.
    pub(crate) fn delete(
        &mut self,
        key: Value,
        mut equal: impl FnMut(Value, Value) -> bool,
    ) -> bool {
        for entry in &mut self.entries {
            if entry.is_some_and(|entry| equal(entry.key, key)) {
                *entry = None;
                self.live_len = self.live_len.saturating_sub(1);
                return true;
            }
        }
        false
    }

    /// Returns the current value for one key without exposing internal storage references.
    pub(crate) fn get(
        &self,
        key: Value,
        mut equal: impl FnMut(Value, Value) -> bool,
    ) -> Option<Value> {
        self.entries
            .iter()
            .flatten()
            .find(|entry| equal(entry.key, key))
            .map(|entry| entry.value)
    }

    /// Reports membership using the same equality callback as Map and Set mutation paths.
    pub(crate) fn has(&self, key: Value, equal: impl FnMut(Value, Value) -> bool) -> bool {
        self.get(key, equal).is_some()
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
        self.entries.capacity() * size_of::<Option<CollectionEntry>>()
    }
}

#[cfg(test)]
mod tests {
    use super::OrderedCollection;
    use tachyon_value::Value;

    #[test]
    fn updates_and_deletes_keep_later_positions_stable() {
        let mut collection = OrderedCollection::default();
        collection
            .insert_or_update(Value::from_i32(1), Value::from_i32(10), |left, right| {
                left == right
            })
            .unwrap();
        collection
            .insert_or_update(Value::from_i32(2), Value::from_i32(20), |left, right| {
                left == right
            })
            .unwrap();
        assert!(collection.delete(Value::from_i32(1), |left, right| left == right));
        assert_eq!(
            collection.get(Value::from_i32(2), |left, right| left == right),
            Some(Value::from_i32(20))
        );
        assert_eq!(collection.len(), 1);
    }
}
