//! Isolate-owned generation slab for long-lived roots and future actor-backed handles.

use core::{
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    num::NonZeroU32,
};

use crate::tuning::{
    CAPACITY_GROWTH_DENOMINATOR, CAPACITY_GROWTH_NUMERATOR, INITIAL_PERSISTENT_ROOT_CAPACITY,
};
use crate::{GcTypeId, HeapReferenceError, RawHeapRef, Trace, Tracer};

/// An isolate-relative root capability; the SDK later pairs it with its owning isolate handle.
///
/// The ID is transportable command data but carries no owner capability: the actor-backed
/// `Persistent<T>` facade must route it to the originating isolate before any table operation.
///
/// ```
/// use tachyon_gc::PersistentRootId;
/// fn assert_send_sync<T: Send + Sync>() {}
/// assert_send_sync::<PersistentRootId<()>>();
/// ```
pub struct PersistentRootId<T: ?Sized> {
    slot: u32,
    generation: NonZeroU32,
    marker: PhantomData<fn() -> T>,
}

impl<T: ?Sized> Copy for PersistentRootId<T> {}

impl<T: ?Sized> Clone for PersistentRootId<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> fmt::Debug for PersistentRootId<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersistentRootId")
            .field("slot", &self.slot)
            .field("generation", &self.generation)
            .finish()
    }
}

impl<T: ?Sized> PartialEq for PersistentRootId<T> {
    fn eq(&self, other: &Self) -> bool {
        self.slot == other.slot && self.generation == other.generation
    }
}

impl<T: ?Sized> Eq for PersistentRootId<T> {}

impl<T: ?Sized> Hash for PersistentRootId<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.slot.hash(state);
        self.generation.hash(state);
    }
}

const _: [(); 8] = [(); core::mem::size_of::<PersistentRootId<()>>()];
const _: [(); 4] = [(); core::mem::align_of::<PersistentRootId<()>>()];

impl<T: ?Sized> PersistentRootId<T> {
    fn new(slot: u32, generation: NonZeroU32) -> Self {
        Self {
            slot,
            generation,
            marker: PhantomData,
        }
    }
}

/// A root command failed without changing any occupied/free slot ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistentRootError {
    EntryLimitExceeded {
        limit: usize,
    },
    AllocationFailed,
    UnknownSlot {
        slot: u32,
    },
    StaleGeneration {
        slot: u32,
        expected: NonZeroU32,
        actual: NonZeroU32,
    },
    VacantSlot {
        slot: u32,
    },
    RetiredSlot {
        slot: u32,
    },
    UnregisteredOrMismatchedType {
        type_id: GcTypeId,
    },
    TypeMismatch {
        expected: GcTypeId,
        actual: GcTypeId,
    },
    InvalidReference(HeapReferenceError),
}

/// Capacity and slot-state evidence for persistent-root tuning and leak diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistentRootStats {
    pub initial_capacity: usize,
    pub growth_count: usize,
    pub peak_live_roots: usize,
    pub live_roots: usize,
    pub free_slots: usize,
    pub retired_slots: usize,
    pub retained_slots: usize,
    pub retained_capacity: usize,
    pub slack_entries: usize,
}

enum RootState {
    Occupied {
        reference: RawHeapRef,
        type_id: GcTypeId,
    },
    Free {
        next: Option<u32>,
    },
    Retired,
}

struct RootEntry {
    generation: NonZeroU32,
    state: RootState,
}

pub(crate) struct PersistentRoots {
    entries: Vec<RootEntry>,
    free_head: Option<u32>,
    max_entries: usize,
    initial_capacity: usize,
    growth_count: usize,
    peak_live_roots: usize,
    live_roots: usize,
    free_slots: usize,
    retired_slots: usize,
}

impl PersistentRoots {
    pub const fn new(max_entries: usize) -> Self {
        Self {
            entries: Vec::new(),
            free_head: None,
            max_entries,
            initial_capacity: 0,
            growth_count: 0,
            peak_live_roots: 0,
            live_roots: 0,
            free_slots: 0,
            retired_slots: 0,
        }
    }

    /// Reuses a free slot or performs one bounded growth step before publishing a root.
    pub fn try_insert<T: ?Sized>(
        &mut self,
        reference: RawHeapRef,
        type_id: GcTypeId,
    ) -> Result<PersistentRootId<T>, PersistentRootError> {
        if let Some(slot) = self.free_head {
            let entry = &mut self.entries[slot as usize];
            let RootState::Free { next } = &entry.state else {
                unreachable!("free-list head always points at a free persistent slot");
            };
            self.free_head = *next;
            self.free_slots -= 1;
            entry.state = RootState::Occupied { reference, type_id };
            let generation = entry.generation;
            self.record_insert();
            return Ok(PersistentRootId::new(slot, generation));
        }
        self.reserve_entry()?;
        let slot = u32::try_from(self.entries.len())
            .expect("persistent root quota is bounded by the logical object count");
        let generation = NonZeroU32::MIN;
        self.entries.push(RootEntry {
            generation,
            state: RootState::Occupied { reference, type_id },
        });
        self.record_insert();
        Ok(PersistentRootId::new(slot, generation))
    }

    /// Allocates an independently releasable slot for an actor-owned handle clone command.
    pub fn try_clone<T: ?Sized>(
        &mut self,
        id: PersistentRootId<T>,
        expected_type: GcTypeId,
    ) -> Result<PersistentRootId<T>, PersistentRootError> {
        let reference = self.resolve(id, expected_type)?;
        self.try_insert(reference, expected_type)
    }

    /// Resolves a live generation and descriptor without exposing table entry storage.
    pub fn resolve<T: ?Sized>(
        &self,
        id: PersistentRootId<T>,
        expected_type: GcTypeId,
    ) -> Result<RawHeapRef, PersistentRootError> {
        let entry = self.entry(id)?;
        match &entry.state {
            RootState::Occupied { reference, type_id } if *type_id == expected_type => {
                Ok(*reference)
            }
            RootState::Occupied { type_id, .. } => Err(PersistentRootError::TypeMismatch {
                expected: expected_type,
                actual: *type_id,
            }),
            RootState::Free { .. } => Err(PersistentRootError::VacantSlot { slot: id.slot }),
            RootState::Retired => Err(PersistentRootError::RetiredSlot { slot: id.slot }),
        }
    }

    /// Invalidates one generation, linking reusable slots without allocating during release.
    pub fn release<T: ?Sized>(
        &mut self,
        id: PersistentRootId<T>,
        expected_type: GcTypeId,
    ) -> Result<(), PersistentRootError> {
        self.resolve(id, expected_type)?;
        let entry = &mut self.entries[id.slot as usize];
        self.live_roots -= 1;
        if let Some(generation) = entry
            .generation
            .get()
            .checked_add(1)
            .and_then(NonZeroU32::new)
        {
            entry.generation = generation;
            entry.state = RootState::Free {
                next: self.free_head,
            };
            self.free_head = Some(id.slot);
            self.free_slots += 1;
        } else {
            entry.state = RootState::Retired;
            self.retired_slots += 1;
        }
        Ok(())
    }

    pub fn stats(&self) -> PersistentRootStats {
        PersistentRootStats {
            initial_capacity: self.initial_capacity,
            growth_count: self.growth_count,
            peak_live_roots: self.peak_live_roots,
            live_roots: self.live_roots,
            free_slots: self.free_slots,
            retired_slots: self.retired_slots,
            retained_slots: self.entries.len(),
            retained_capacity: self.entries.capacity(),
            slack_entries: self.entries.capacity() - self.entries.len(),
        }
    }

    fn entry<T: ?Sized>(&self, id: PersistentRootId<T>) -> Result<&RootEntry, PersistentRootError> {
        let entry = self
            .entries
            .get(id.slot as usize)
            .ok_or(PersistentRootError::UnknownSlot { slot: id.slot })?;
        if entry.generation != id.generation {
            return Err(PersistentRootError::StaleGeneration {
                slot: id.slot,
                expected: entry.generation,
                actual: id.generation,
            });
        }
        Ok(entry)
    }

    /// Applies the centralized bounded 1.5x policy before an append can publish a new slot.
    fn reserve_entry(&mut self) -> Result<(), PersistentRootError> {
        if self.entries.len() == self.max_entries {
            return Err(PersistentRootError::EntryLimitExceeded {
                limit: self.max_entries,
            });
        }
        if self.entries.len() < self.entries.capacity() {
            return Ok(());
        }
        let target = if self.entries.capacity() == 0 {
            INITIAL_PERSISTENT_ROOT_CAPACITY.min(self.max_entries)
        } else {
            self.entries
                .capacity()
                .saturating_mul(CAPACITY_GROWTH_NUMERATOR)
                .div_ceil(CAPACITY_GROWTH_DENOMINATOR)
                .min(self.max_entries)
        }
        .max(self.entries.len() + 1);
        self.entries
            .try_reserve_exact(target - self.entries.len())
            .map_err(|_| PersistentRootError::AllocationFailed)?;
        if self.initial_capacity == 0 {
            self.initial_capacity = self.entries.capacity();
        } else {
            self.growth_count += 1;
        }
        Ok(())
    }

    fn record_insert(&mut self) {
        self.live_roots += 1;
        self.peak_live_roots = self.peak_live_roots.max(self.live_roots);
    }
}

impl Trace for PersistentRoots {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        for entry in &mut self.entries {
            if let RootState::Occupied { reference, .. } = &mut entry.state {
                tracer.trace_raw_heap_ref(reference);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;

    use tachyon_value::Value;

    use super::{PersistentRootError, PersistentRoots, RootState};
    use crate::{GcTypeId, RawHeapRef, Trace, Tracer};

    struct Object;

    fn type_id() -> GcTypeId {
        GcTypeId::new(1).unwrap()
    }

    #[test]
    /// Reuses a released slot at a new generation and rejects every stale copy of the old ID.
    fn released_slots_reuse_indices_without_generation_aba() {
        let mut roots = PersistentRoots::new(2);
        let first = roots
            .try_insert::<Object>(RawHeapRef::new(16).unwrap(), type_id())
            .unwrap();
        roots.release(first, type_id()).unwrap();
        let second = roots
            .try_insert::<Object>(RawHeapRef::new(32).unwrap(), type_id())
            .unwrap();

        assert_eq!(first.slot, second.slot);
        assert_ne!(first.generation, second.generation);
        assert!(matches!(
            roots.resolve(first, type_id()),
            Err(PersistentRootError::StaleGeneration { slot: 0, .. })
        ));
        assert_eq!(
            roots.resolve(second, type_id()).unwrap(),
            RawHeapRef::new(32).unwrap()
        );
    }

    #[test]
    /// Exhausted generations retire permanently instead of wrapping into a stale valid ID.
    fn maximum_generation_retires_the_slot() {
        let mut roots = PersistentRoots::new(2);
        let original = roots
            .try_insert::<Object>(RawHeapRef::new(16).unwrap(), type_id())
            .unwrap();
        roots.entries[0].generation = NonZeroU32::MAX;
        let maximum = super::PersistentRootId::<Object>::new(0, NonZeroU32::MAX);

        roots.release(maximum, type_id()).unwrap();
        assert!(matches!(roots.entries[0].state, RootState::Retired));
        let replacement = roots
            .try_insert::<Object>(RawHeapRef::new(32).unwrap(), type_id())
            .unwrap();
        assert_eq!(replacement.slot, 1);
        assert!(matches!(
            roots.resolve(original, type_id()),
            Err(PersistentRootError::StaleGeneration { slot: 0, .. })
        ));
        assert_eq!(roots.stats().retired_slots, 1);
    }

    #[test]
    fn root_table_growth_and_quota_are_explicit() {
        let mut roots = PersistentRoots::new(100);
        for offset in 1..=100 {
            roots
                .try_insert::<Object>(RawHeapRef::new(offset).unwrap(), type_id())
                .unwrap();
        }
        assert!(matches!(
            roots.try_insert::<Object>(RawHeapRef::new(101).unwrap(), type_id()),
            Err(PersistentRootError::EntryLimitExceeded { limit: 100 })
        ));
        let stats = roots.stats();
        assert_eq!(stats.initial_capacity, 64);
        assert_eq!(stats.growth_count, 2);
        assert_eq!(stats.peak_live_roots, 100);
        assert_eq!(stats.retained_capacity, 100);
    }

    #[test]
    fn clone_quota_failure_preserves_the_original_root() {
        let mut roots = PersistentRoots::new(1);
        let original = roots
            .try_insert::<Object>(RawHeapRef::new(16).unwrap(), type_id())
            .unwrap();

        assert_eq!(
            roots.try_clone(original, type_id()),
            Err(PersistentRootError::EntryLimitExceeded { limit: 1 })
        );
        assert_eq!(
            roots.resolve(original, type_id()).unwrap(),
            RawHeapRef::new(16).unwrap()
        );
        assert_eq!(roots.stats().live_roots, 1);
    }

    #[test]
    fn descriptor_mismatch_does_not_release_the_slot() {
        let mut roots = PersistentRoots::new(1);
        let original = roots
            .try_insert::<Object>(RawHeapRef::new(16).unwrap(), type_id())
            .unwrap();
        let other = GcTypeId::new(2).unwrap();

        assert_eq!(
            roots.release(original, other),
            Err(PersistentRootError::TypeMismatch {
                expected: other,
                actual: type_id(),
            })
        );
        assert_eq!(roots.stats().live_roots, 1);
        assert_eq!(roots.resolve(original, type_id()).unwrap().offset(), 16);
    }

    struct RewritingTracer;

    impl Tracer for RewritingTracer {
        fn trace_value(&mut self, _: &mut Value) {}

        fn trace_raw_heap_ref(&mut self, reference: &mut RawHeapRef) {
            *reference = RawHeapRef::new(reference.offset() + 16).unwrap();
        }

        fn trace_weak_raw_heap_ref(&mut self, reference: &mut Option<RawHeapRef>) {
            *reference =
                reference.map(|reference| RawHeapRef::new(reference.offset() + 16).unwrap());
        }

        fn trace_ephemeron(&mut self, key: &mut Option<RawHeapRef>, _: &mut Value) {
            self.trace_weak_raw_heap_ref(key);
        }

        fn trace_finalization(&mut self, target: &mut Option<RawHeapRef>, _: &mut Value) {
            self.trace_weak_raw_heap_ref(target);
        }
    }

    #[test]
    fn occupied_persistent_slots_follow_rewrite_capable_tracing() {
        let mut roots = PersistentRoots::new(1);
        let id = roots
            .try_insert::<Object>(RawHeapRef::new(16).unwrap(), type_id())
            .unwrap();

        roots.trace(&mut RewritingTracer);

        assert_eq!(
            roots.resolve(id, type_id()).unwrap(),
            RawHeapRef::new(32).unwrap()
        );
    }
}
