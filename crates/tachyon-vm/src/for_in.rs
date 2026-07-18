//! GC-managed `for-in` snapshots and allocation-bounded duplicate suppression.

use core::mem::size_of;

use tachyon_gc::{GcExternalMemory, Trace, Tracer};

use crate::{AtomId, tuning::objects};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ForInAllocationError {
    CapacityOverflow,
    AllocationFailed,
}

/// One internal iterator rooted by the bytecode register that owns it.
#[derive(Debug)]
pub(crate) struct ForInIterator {
    keys: Box<[AtomId]>,
    index: u32,
}

impl ForInIterator {
    pub(crate) const fn new(keys: Box<[AtomId]>) -> Self {
        Self { keys, index: 0 }
    }

    #[inline(always)]
    pub(crate) fn next(&mut self) -> Option<AtomId> {
        let key = self.keys.get(self.index as usize).copied()?;
        self.index += 1;
        Some(key)
    }
}

impl Trace for ForInIterator {
    #[inline(always)]
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl GcExternalMemory for ForInIterator {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.keys.len() * size_of::<AtomId>()
    }
}

/// Preserves discovery order while suppressing prototype duplicates in expected O(1) time.
pub(crate) struct ForInKeySet {
    keys: Vec<AtomId>,
    buckets: Vec<Option<AtomId>>,
}

impl ForInKeySet {
    /// Uses the pre-scanned property count so neither vector grows while collecting the snapshot.
    pub(crate) fn with_upper_bound(upper_bound: usize) -> Result<Self, ForInAllocationError> {
        let bucket_target = upper_bound
            .checked_mul(objects::FOR_IN_SEEN_LOAD_DENOMINATOR)
            .ok_or(ForInAllocationError::CapacityOverflow)?;
        let bucket_capacity = bucket_target
            .max(objects::MIN_FOR_IN_SEEN_CAPACITY)
            .checked_next_power_of_two()
            .ok_or(ForInAllocationError::CapacityOverflow)?;
        let mut keys = Vec::new();
        keys.try_reserve_exact(upper_bound)
            .map_err(|_| ForInAllocationError::AllocationFailed)?;
        let mut buckets = Vec::new();
        buckets
            .try_reserve_exact(bucket_capacity)
            .map_err(|_| ForInAllocationError::AllocationFailed)?;
        buckets.resize(bucket_capacity, None);
        Ok(Self { keys, buckets })
    }

    /// Returns true only for the first occurrence of one isolate-local atom.
    #[inline]
    pub(crate) fn insert(&mut self, key: AtomId) -> bool {
        let mask = self.buckets.len() - 1;
        let mut bucket =
            (key.index() as usize).wrapping_mul(objects::FOR_IN_ATOM_HASH_MULTIPLIER) & mask;
        loop {
            match self.buckets[bucket] {
                Some(existing) if existing == key => return false,
                Some(_) => bucket = (bucket + 1) & mask,
                None => {
                    self.buckets[bucket] = Some(key);
                    return true;
                }
            }
        }
    }

    #[inline]
    pub(crate) fn push_enumerable(&mut self, key: AtomId) {
        debug_assert!(self.keys.len() < self.keys.capacity());
        self.keys.push(key);
    }

    pub(crate) fn finish(self) -> Box<[AtomId]> {
        self.keys.into_boxed_slice()
    }
}

#[cfg(test)]
mod tests {
    use super::ForInKeySet;
    use crate::AtomId;

    #[test]
    fn key_set_preserves_first_occurrence_without_growth() {
        let mut keys = ForInKeySet::with_upper_bound(3).unwrap();
        let first = AtomId::from_test_index(0);
        let second = AtomId::from_test_index(17);
        assert!(keys.insert(first));
        keys.push_enumerable(first);
        assert!(keys.insert(second));
        keys.push_enumerable(second);
        assert!(!keys.insert(first));
        assert_eq!(&*keys.finish(), &[first, second]);
    }
}
