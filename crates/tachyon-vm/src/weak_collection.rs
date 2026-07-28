//! Ephemeron-backed private storage for WeakMap and WeakSet.

use core::mem::size_of;

use tachyon_gc::{Ephemeron, GcExternalMemory, GcRef, Trace, Tracer, WeakGcRef};
use tachyon_value::{RawHeapRef, Value};

use crate::object::OrdinaryObject;
use crate::tuning;

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
    buckets: Box<[u32]>,
    next_free: Box<[u32]>,
    free_head: u32,
}

const EMPTY_BUCKET: u32 = 0;
const TOMBSTONE_BUCKET: u32 = u32::MAX;

impl WeakCollection {
    /// Creates an exactly charged ephemeron table with fallible backing allocation.
    pub(crate) fn with_capacity(capacity: usize) -> Result<Self, ()> {
        if capacity != 0 && !capacity.is_power_of_two() {
            return Err(());
        }
        let bucket_capacity = capacity.checked_mul(2).ok_or(())?;
        let mut entries = Vec::new();
        entries.try_reserve_exact(capacity).map_err(|_| ())?;
        entries.resize(capacity, None);
        let mut buckets = Vec::new();
        buckets.try_reserve_exact(bucket_capacity).map_err(|_| ())?;
        buckets.resize(bucket_capacity, EMPTY_BUCKET);
        let mut next_free = Vec::new();
        next_free.try_reserve_exact(capacity).map_err(|_| ())?;
        next_free.extend(
            (0..capacity)
                .map(|index| encode_optional_index((index + 1 < capacity).then_some(index + 1))),
        );
        Ok(Self {
            entries: entries.into_boxed_slice(),
            buckets: buckets.into_boxed_slice(),
            next_free: next_free.into_boxed_slice(),
            free_head: encode_optional_index((capacity != 0).then_some(0)),
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

    /// Finds a live key through bounded linear probing over its stable logical address hash.
    #[inline(always)]
    pub(crate) fn find_index(&self, key: RawHeapRef) -> Option<usize> {
        let bucket_capacity = self.buckets.len();
        if bucket_capacity == 0 {
            return None;
        }
        let start = weak_key_bucket(key, bucket_capacity);
        for distance in 0..bucket_capacity {
            let bucket = self.buckets[(start + distance) & (bucket_capacity - 1)];
            match decode_bucket_index(bucket) {
                BucketIndex::Empty => return None,
                BucketIndex::Entry(index)
                    if self.entries[index].is_some_and(|entry| {
                        entry.key().is_some_and(|current| current.raw() == key)
                    }) =>
                {
                    return Some(index);
                }
                BucketIndex::Entry(_) | BucketIndex::Tombstone => {}
            }
        }
        None
    }

    /// Returns the key's first reusable tombstone or never-used slot in its probe chain.
    #[inline(always)]
    pub(crate) fn insertion_index(&self, key: RawHeapRef) -> Option<usize> {
        let _ = self.insertion_bucket(key)?;
        decode_optional_index(self.free_head)
    }

    /// Publishes a new entry into an empty or collector-cleared slot.
    pub(crate) fn install_at(&mut self, index: usize, entry: Ephemeron<()>) -> Result<(), ()> {
        if self
            .entries
            .get(index)
            .ok_or(())?
            .as_ref()
            .is_some_and(|current| current.key().is_some())
        {
            return Err(());
        }
        if decode_optional_index(self.free_head) != Some(index) {
            return Err(());
        }
        let key = entry.key().ok_or(())?.raw();
        let bucket = self.insertion_bucket(key).ok_or(())?;
        self.free_head = self.next_free[index];
        self.next_free[index] = EMPTY_BUCKET;
        self.entries[index] = Some(entry);
        self.buckets[bucket] = encode_entry_index(index)?;
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
        let key = self
            .entries
            .get(index)
            .and_then(Option::as_ref)
            .and_then(Ephemeron::key)
            .ok_or(())?
            .raw();
        let bucket = self.find_bucket(key).ok_or(())?;
        self.entries[index].as_mut().ok_or(())?.clear();
        self.buckets[bucket] = TOMBSTONE_BUCKET;
        self.next_free[index] = self.free_head;
        self.free_head = encode_optional_index(Some(index));
        Ok(())
    }

    /// Rehashes live entries into a larger exact-size backing and discards cleared tombstones.
    pub(crate) fn grow_copy(&self, capacity: usize) -> Result<Self, ()> {
        if capacity < self.capacity() {
            return Err(());
        }
        let mut grown = Self::with_capacity(capacity)?;
        for entry in self.entries.iter().flatten().copied() {
            let Some(key) = entry.key() else {
                continue;
            };
            let index = grown.insertion_index(key.raw()).ok_or(())?;
            grown.install_at(index, entry)?;
        }
        Ok(grown)
    }

    /// Rebuilds non-GC metadata after a weak phase may have cleared ephemeron keys.
    fn rebuild_index_in_place(&mut self) {
        self.buckets.fill(EMPTY_BUCKET);
        self.free_head = EMPTY_BUCKET;
        for index in (0..self.entries.len()).rev() {
            let Some(key) = self.entries[index].and_then(|entry| entry.key()) else {
                self.next_free[index] = self.free_head;
                self.free_head = encode_optional_index(Some(index));
                continue;
            };
            self.next_free[index] = EMPTY_BUCKET;
            if let Some(bucket) = self.insertion_bucket(key.raw()) {
                self.buckets[bucket] = encode_entry_index(index).expect("capacity fits u32");
            }
        }
    }

    #[inline(always)]
    fn find_bucket(&self, key: RawHeapRef) -> Option<usize> {
        let capacity = self.buckets.len();
        let start = weak_key_bucket(key, capacity);
        for distance in 0..capacity {
            let bucket_index = (start + distance) & (capacity - 1);
            match decode_bucket_index(self.buckets[bucket_index]) {
                BucketIndex::Empty => return None,
                BucketIndex::Entry(index)
                    if self.entries[index].is_some_and(|entry| {
                        entry.key().is_some_and(|current| current.raw() == key)
                    }) =>
                {
                    return Some(bucket_index);
                }
                BucketIndex::Entry(_) | BucketIndex::Tombstone => {}
            }
        }
        None
    }

    #[inline(always)]
    fn insertion_bucket(&self, key: RawHeapRef) -> Option<usize> {
        let capacity = self.buckets.len();
        if capacity == 0 {
            return None;
        }
        let start = weak_key_bucket(key, capacity);
        let mut tombstone = None;
        for distance in 0..capacity {
            let index = (start + distance) & (capacity - 1);
            match decode_bucket_index(self.buckets[index]) {
                BucketIndex::Empty => return Some(tombstone.unwrap_or(index)),
                BucketIndex::Tombstone => {
                    tombstone.get_or_insert(index);
                }
                BucketIndex::Entry(_) => {}
            }
        }
        tombstone
    }
}

enum BucketIndex {
    Empty,
    Tombstone,
    Entry(usize),
}

#[inline(always)]
fn encode_optional_index(index: Option<usize>) -> u32 {
    index.map_or(EMPTY_BUCKET, |index| (index as u32) + 1)
}

#[inline(always)]
fn decode_optional_index(encoded: u32) -> Option<usize> {
    (encoded != EMPTY_BUCKET).then_some(encoded.saturating_sub(1) as usize)
}

#[inline(always)]
fn encode_entry_index(index: usize) -> Result<u32, ()> {
    u32::try_from(index)
        .ok()
        .and_then(|index| index.checked_add(1))
        .ok_or(())
}

#[inline(always)]
fn decode_bucket_index(encoded: u32) -> BucketIndex {
    match encoded {
        EMPTY_BUCKET => BucketIndex::Empty,
        TOMBSTONE_BUCKET => BucketIndex::Tombstone,
        index => BucketIndex::Entry((index - 1) as usize),
    }
}

/// Avalanches both span-id and in-span offset bits before masking into a power-of-two table.
#[inline(always)]
fn weak_key_bucket(key: RawHeapRef, capacity: usize) -> usize {
    let mut mixed = key.offset();
    mixed ^= mixed >> 16;
    mixed = mixed.wrapping_mul(tuning::collections::WEAK_KEY_HASH_MULTIPLIER_1);
    mixed ^= mixed >> 15;
    mixed = mixed.wrapping_mul(tuning::collections::WEAK_KEY_HASH_MULTIPLIER_2);
    mixed ^= mixed >> 16;
    mixed as usize & (capacity - 1)
}

impl Trace for WeakCollection {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.entries.trace(tracer);
        self.rebuild_index_in_place();
    }
}

impl GcExternalMemory for WeakCollection {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.entries.len() * size_of::<Option<Ephemeron<()>>>()
            + self.buckets.len() * size_of::<u32>()
            + self.next_free.len() * size_of::<u32>()
    }
}

impl Default for WeakCollection {
    fn default() -> Self {
        Self::with_capacity(0).expect("zero-capacity weak collection never allocates")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tachyon_gc::{Ephemeron, GcRef};
    use tachyon_value::{RawHeapRef, SpanId, SpanOffset, Value};

    use super::{WeakCollection, weak_key_bucket};

    fn key(offset: u32) -> RawHeapRef {
        RawHeapRef::new(offset).expect("test key uses a non-zero in-span offset")
    }

    fn insert(table: &mut WeakCollection, raw: RawHeapRef, value: i32) -> usize {
        let index = table.insertion_index(raw).expect("table has a slot");
        table
            .install_at(
                index,
                Ephemeron::new(GcRef::from_erased_raw(raw), Value::from_i32(value)),
            )
            .expect("slot is reusable");
        index
    }

    fn key_ordinal(ordinal: usize) -> RawHeapRef {
        const SLOTS_PER_SPAN: usize = (u16::MAX as usize) / 16;
        let span = ordinal / SLOTS_PER_SPAN;
        let slot = ordinal % SLOTS_PER_SPAN + 1;
        RawHeapRef::from_parts(
            SpanId::new(span as u16),
            SpanOffset::new((slot * 16) as u16).expect("slot offset is non-zero"),
        )
    }

    fn colliding_key(first: RawHeapRef, capacity: usize) -> RawHeapRef {
        (32..u16::MAX as u32)
            .step_by(16)
            .map(key)
            .find(|candidate| {
                *candidate != first
                    && weak_key_bucket(*candidate, capacity) == weak_key_bucket(first, capacity)
            })
            .expect("bounded bucket count guarantees a collision")
    }

    #[test]
    fn probing_finds_collisions_and_preserves_the_chain_across_delete() {
        let mut table = WeakCollection::with_capacity(4).unwrap();
        let first = key(16);
        let collision = colliding_key(first, 4);
        let first_index = insert(&mut table, first, 1);
        let collision_index = insert(&mut table, collision, 2);

        assert_ne!(first_index, collision_index);
        assert_eq!(table.find_index(first), Some(first_index));
        assert_eq!(table.find_index(collision), Some(collision_index));
        table.delete_at(first_index).unwrap();
        assert_eq!(table.find_index(first), None);
        assert_eq!(table.find_index(collision), Some(collision_index));
    }

    #[test]
    fn collector_cleared_slot_is_reused_as_a_tombstone() {
        let mut table = WeakCollection::with_capacity(4).unwrap();
        let cleared = key(16);
        let replacement = colliding_key(cleared, 4);
        let index = insert(&mut table, cleared, 1);
        table.entries[index].as_mut().unwrap().clear();
        table.rebuild_index_in_place();

        assert_eq!(table.find_index(cleared), None);
        assert!(table.insertion_index(replacement).is_some());
        insert(&mut table, replacement, 2);
        assert!(table.find_index(replacement).is_some());
    }

    #[test]
    fn growth_rehashes_live_entries_and_drops_tombstones() {
        let mut table = WeakCollection::with_capacity(4).unwrap();
        let deleted = key(16);
        let retained = key(80);
        let deleted_index = insert(&mut table, deleted, 1);
        insert(&mut table, retained, 2);
        table.delete_at(deleted_index).unwrap();

        let grown = table.grow_copy(8).unwrap();
        assert_eq!(grown.find_index(deleted), None);
        let retained = grown.find_index(retained).expect("live entry was rehashed");
        assert_eq!(grown.entry_at(retained).unwrap().value().as_i32(), Some(2));
        assert_eq!(grown.entries.iter().flatten().count(), 1);
    }

    #[test]
    fn hundred_thousand_entry_table_retains_every_key() {
        const ENTRY_COUNT: usize = 100_000;
        let mut table = WeakCollection::with_capacity(ENTRY_COUNT.next_power_of_two()).unwrap();
        for ordinal in 0..ENTRY_COUNT {
            insert(&mut table, key_ordinal(ordinal), ordinal as i32);
        }
        for ordinal in 0..ENTRY_COUNT {
            let index = table
                .find_index(key_ordinal(ordinal))
                .expect("inserted key remains reachable");
            assert_eq!(
                table.entry_at(index).unwrap().value().as_i32(),
                Some(ordinal as i32)
            );
        }
    }

    #[test]
    fn hash_avalanches_span_identity_at_small_capacities() {
        let buckets = (0..128_u16)
            .map(|span| {
                weak_key_bucket(
                    RawHeapRef::from_parts(
                        SpanId::new(span),
                        SpanOffset::new(16).expect("non-zero slot offset"),
                    ),
                    256,
                )
            })
            .collect::<BTreeSet<_>>();
        assert!(
            buckets.len() >= 96,
            "only {} distinct buckets",
            buckets.len()
        );
    }
}
