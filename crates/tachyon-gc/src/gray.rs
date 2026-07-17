//! Bounded high-water storage for iterative tri-color traversal.

use crate::{
    RawHeapRef,
    tuning::{CAPACITY_GROWTH_DENOMINATOR, CAPACITY_GROWTH_NUMERATOR, INITIAL_GRAY_QUEUE_CAPACITY},
};

/// A gray edge could not be retained without violating the collector quota.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GrayQueueError {
    EntryLimitExceeded { limit: usize },
    AllocationFailed,
}

/// Capacity evidence retained for tuning and low-memory tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GrayQueueStats {
    pub initial_capacity: usize,
    pub growth_count: usize,
    pub peak_len: usize,
    pub retained_capacity: usize,
    pub slack_entries: usize,
}

/// A LIFO gray worklist; mark bits, not duplicate queue scans, enforce one enqueue per epoch.
pub(crate) struct GrayQueue {
    entries: Vec<RawHeapRef>,
    max_entries: usize,
    initial_capacity: usize,
    growth_count: usize,
    peak_len: usize,
}

impl GrayQueue {
    pub const fn new(max_entries: usize) -> Self {
        Self {
            entries: Vec::new(),
            max_entries,
            initial_capacity: 0,
            growth_count: 0,
            peak_len: 0,
        }
    }

    /// Clears logical work while retaining the historical high-water allocation.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Reserves before the caller sets a mark bit, preventing marked-but-not-enqueued objects.
    pub fn try_reserve_one(&mut self) -> Result<(), GrayQueueError> {
        if self.entries.len() == self.max_entries {
            return Err(GrayQueueError::EntryLimitExceeded {
                limit: self.max_entries,
            });
        }
        if self.entries.len() < self.entries.capacity() {
            return Ok(());
        }
        let target = if self.entries.capacity() == 0 {
            INITIAL_GRAY_QUEUE_CAPACITY.min(self.max_entries)
        } else {
            self.entries
                .capacity()
                .saturating_mul(CAPACITY_GROWTH_NUMERATOR)
                .div_ceil(CAPACITY_GROWTH_DENOMINATOR)
                .min(self.max_entries)
        };
        let target = target.max(self.entries.len() + 1);
        self.entries
            .try_reserve_exact(target - self.entries.len())
            .map_err(|_| GrayQueueError::AllocationFailed)?;
        if self.initial_capacity == 0 {
            self.initial_capacity = self.entries.capacity();
        } else {
            self.growth_count += 1;
        }
        Ok(())
    }

    /// Pushes after capacity and mark-bit publication have succeeded.
    pub fn push_reserved(&mut self, reference: RawHeapRef) {
        debug_assert!(self.entries.len() < self.entries.capacity());
        self.entries.push(reference);
        self.peak_len = self.peak_len.max(self.entries.len());
    }

    pub fn pop(&mut self) -> Option<RawHeapRef> {
        self.entries.pop()
    }

    pub fn stats(&self) -> GrayQueueStats {
        GrayQueueStats {
            initial_capacity: self.initial_capacity,
            growth_count: self.growth_count,
            peak_len: self.peak_len,
            retained_capacity: self.entries.capacity(),
            slack_entries: self.entries.capacity() - self.entries.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GrayQueue, GrayQueueError};
    use crate::RawHeapRef;

    #[test]
    /// Crosses the initial hint, retains high water after clear, and enforces the exact quota.
    fn queue_growth_and_limit_are_explicit_and_stable() {
        let limit = 300;
        let mut queue = GrayQueue::new(limit);
        for offset in 1..=limit {
            queue.try_reserve_one().unwrap();
            queue.push_reserved(
                RawHeapRef::new(offset as u32).expect("test offsets have non-zero low bits"),
            );
        }
        assert_eq!(
            queue.try_reserve_one(),
            Err(GrayQueueError::EntryLimitExceeded { limit })
        );
        let stats = queue.stats();
        assert_eq!(stats.initial_capacity, 256);
        assert_eq!(stats.growth_count, 1);
        assert_eq!(stats.peak_len, limit);
        assert_eq!(stats.retained_capacity, limit);
        assert_eq!(stats.slack_entries, 0);

        queue.clear();
        assert_eq!(queue.stats().retained_capacity, limit);
        assert_eq!(queue.stats().slack_entries, limit);
    }
}
