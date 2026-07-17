//! Heap-owned temporary roots and exact composition with subsystem root providers.

use crate::tuning::{
    CAPACITY_GROWTH_DENOMINATOR, CAPACITY_GROWTH_NUMERATOR, INITIAL_TEMPORARY_ROOT_CAPACITY,
};
use crate::{RawHeapRef, Trace, Tracer};

/// A temporary root cannot be published within the isolate's bounded entry quota.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TemporaryRootError {
    EntryLimitExceeded { limit: usize },
    AllocationFailed,
}

/// Retained high-water evidence for temporary root capacity tuning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TemporaryRootStats {
    pub initial_capacity: usize,
    pub growth_count: usize,
    pub peak_len: usize,
    pub current_len: usize,
    pub retained_capacity: usize,
    pub slack_entries: usize,
}

/// A reusable LIFO root stack whose logical lifetime is controlled by scope checkpoints.
pub(crate) struct TemporaryRoots {
    entries: Vec<RawHeapRef>,
    max_entries: usize,
    initial_capacity: usize,
    growth_count: usize,
    peak_len: usize,
}

impl TemporaryRoots {
    pub const fn new(max_entries: usize) -> Self {
        Self {
            entries: Vec::new(),
            max_entries,
            initial_capacity: 0,
            growth_count: 0,
            peak_len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Reserves one bounded 1.5x growth step before publishing the root entry.
    pub fn try_push(&mut self, reference: RawHeapRef) -> Result<(), TemporaryRootError> {
        if self.entries.len() == self.max_entries {
            return Err(TemporaryRootError::EntryLimitExceeded {
                limit: self.max_entries,
            });
        }
        if self.entries.len() == self.entries.capacity() {
            let target = if self.entries.capacity() == 0 {
                INITIAL_TEMPORARY_ROOT_CAPACITY.min(self.max_entries)
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
                .map_err(|_| TemporaryRootError::AllocationFailed)?;
            if self.initial_capacity == 0 {
                self.initial_capacity = self.entries.capacity();
            } else {
                self.growth_count += 1;
            }
        }
        self.entries.push(reference);
        self.peak_len = self.peak_len.max(self.entries.len());
        Ok(())
    }

    /// Rolls back a completed or unwound scope without releasing reusable high-water storage.
    pub fn truncate(&mut self, checkpoint: usize) {
        debug_assert!(checkpoint <= self.entries.len());
        self.entries.truncate(checkpoint);
    }

    pub fn stats(&self) -> TemporaryRootStats {
        TemporaryRootStats {
            initial_capacity: self.initial_capacity,
            growth_count: self.growth_count,
            peak_len: self.peak_len,
            current_len: self.entries.len(),
            retained_capacity: self.entries.capacity(),
            slack_entries: self.entries.capacity() - self.entries.len(),
        }
    }
}

impl Trace for TemporaryRoots {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.entries.trace(tracer);
    }
}

/// Visits subsystem roots and scope roots through one exact strong-root contract.
pub(crate) struct RootComposition<'a> {
    external: &'a mut dyn Trace,
    temporary: &'a mut TemporaryRoots,
}

impl<'a> RootComposition<'a> {
    pub const fn new(external: &'a mut dyn Trace, temporary: &'a mut TemporaryRoots) -> Self {
        Self {
            external,
            temporary,
        }
    }
}

impl Trace for RootComposition<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.external.trace(tracer);
        self.temporary.trace(tracer);
    }
}

#[cfg(test)]
mod tests {
    use super::{TemporaryRootError, TemporaryRoots};
    use crate::{RawHeapRef, Trace, Tracer};
    use tachyon_value::Value;

    #[test]
    fn temporary_root_growth_retains_high_water_and_enforces_quota() {
        let limit = 100;
        let mut roots = TemporaryRoots::new(limit);
        for offset in 1..=limit {
            roots
                .try_push(RawHeapRef::new(offset as u32).unwrap())
                .unwrap();
        }
        assert_eq!(
            roots.try_push(RawHeapRef::new(101).unwrap()),
            Err(TemporaryRootError::EntryLimitExceeded { limit })
        );
        let stats = roots.stats();
        assert_eq!(stats.initial_capacity, 64);
        assert_eq!(stats.growth_count, 2);
        assert_eq!(stats.peak_len, limit);
        assert_eq!(stats.retained_capacity, limit);

        roots.truncate(25);
        let retained = roots.stats();
        assert_eq!(retained.current_len, 25);
        assert_eq!(retained.slack_entries, 75);
        assert_eq!(retained.retained_capacity, limit);
    }

    struct RewritingTracer;

    impl Tracer for RewritingTracer {
        fn trace_value(&mut self, _: &mut Value) {}

        fn trace_raw_heap_ref(&mut self, reference: &mut RawHeapRef) {
            *reference = RawHeapRef::new(reference.offset() + 16).unwrap();
        }
    }

    #[test]
    fn temporary_roots_follow_the_rewrite_capable_trace_contract() {
        let mut roots = TemporaryRoots::new(1);
        roots.try_push(RawHeapRef::new(16).unwrap()).unwrap();

        roots.trace(&mut RewritingTracer);

        assert_eq!(roots.entries, [RawHeapRef::new(32).unwrap()]);
    }
}
