//! Finalization registration edges and pending cleanup records rooted outside collector callbacks.

use core::marker::PhantomData;
use std::collections::VecDeque;

use tachyon_value::{RawHeapRef, Value};

use crate::tuning::{
    CAPACITY_GROWTH_DENOMINATOR, CAPACITY_GROWTH_NUMERATOR, INITIAL_FINALIZATION_QUEUE_CAPACITY,
};
use crate::{GcRef, Trace, Tracer};

/// A weak target plus strongly held cleanup value owned by a live registry object.
#[derive(Debug)]
pub struct FinalizationRegistration<T: ?Sized> {
    target: Option<RawHeapRef>,
    held_value: Value,
    marker: PhantomData<fn() -> T>,
}

impl<T: ?Sized> FinalizationRegistration<T> {
    #[must_use]
    pub const fn new(target: GcRef<T>, held_value: Value) -> Self {
        Self {
            target: Some(target.raw()),
            held_value,
            marker: PhantomData,
        }
    }

    #[must_use]
    pub fn target(&self) -> Option<GcRef<T>> {
        self.target.map(GcRef::from_raw)
    }

    #[must_use]
    pub const fn held_value(&self) -> Value {
        self.held_value
    }
}

impl<T: ?Sized> Trace for FinalizationRegistration<T> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        tracer.trace_finalization(&mut self.target, &mut self.held_value);
    }
}

/// One cleanup command retained as a strong root until a post-collection safepoint consumes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingFinalization {
    registry: RawHeapRef,
    held_value: Value,
}

impl PendingFinalization {
    #[must_use]
    pub const fn registry(self) -> RawHeapRef {
        self.registry
    }

    #[must_use]
    pub const fn held_value(self) -> Value {
        self.held_value
    }
}

impl Trace for PendingFinalization {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.registry.trace(tracer);
        self.held_value.trace(tracer);
    }
}

/// A pending cleanup record could not be published within the isolate object quota.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalizationQueueError {
    EntryLimitExceeded { limit: usize },
    AllocationFailed,
}

/// Pending cleanup and retained-capacity evidence for safepoint scheduling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalizationQueueStats {
    pub pending: usize,
    pub initial_capacity: usize,
    pub growth_count: usize,
    pub peak_len: usize,
    pub retained_capacity: usize,
    pub slack_entries: usize,
}

pub(crate) struct PendingFinalizations {
    entries: VecDeque<PendingFinalization>,
    max_entries: usize,
    initial_capacity: usize,
    growth_count: usize,
    peak_len: usize,
}

impl PendingFinalizations {
    pub const fn new(max_entries: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_entries,
            initial_capacity: 0,
            growth_count: 0,
            peak_len: 0,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn get(&self, index: usize) -> Option<PendingFinalization> {
        self.entries.get(index).copied()
    }

    /// Removes one record only when the post-collection cleanup scheduler is ready to own it.
    pub(crate) fn pop(&mut self) -> Option<PendingFinalization> {
        self.entries.pop_front()
    }

    /// Reserves queue capacity before publishing a record whose registration will then be cleared.
    pub fn try_enqueue(
        &mut self,
        registry: RawHeapRef,
        held_value: Value,
    ) -> Result<(), FinalizationQueueError> {
        if self.entries.len() == self.max_entries {
            return Err(FinalizationQueueError::EntryLimitExceeded {
                limit: self.max_entries,
            });
        }
        if self.entries.len() == self.entries.capacity() {
            let target = if self.entries.capacity() == 0 {
                INITIAL_FINALIZATION_QUEUE_CAPACITY.min(self.max_entries)
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
                .map_err(|_| FinalizationQueueError::AllocationFailed)?;
            if self.initial_capacity == 0 {
                self.initial_capacity = self.entries.capacity();
            } else {
                self.growth_count += 1;
            }
        }
        self.entries.push_back(PendingFinalization {
            registry,
            held_value,
        });
        self.peak_len = self.peak_len.max(self.entries.len());
        Ok(())
    }

    #[must_use]
    pub fn stats(&self) -> FinalizationQueueStats {
        FinalizationQueueStats {
            pending: self.entries.len(),
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
    use super::{FinalizationQueueError, PendingFinalizations};
    use crate::RawHeapRef;
    use tachyon_value::Value;

    #[test]
    fn pending_queue_growth_and_limit_are_explicit() {
        let mut queue = PendingFinalizations::new(100);
        for offset in 1..=100 {
            queue
                .try_enqueue(
                    RawHeapRef::new(offset).unwrap(),
                    Value::from_i32(offset as i32),
                )
                .unwrap();
        }
        assert_eq!(
            queue.try_enqueue(RawHeapRef::new(101).unwrap(), Value::from_i32(101)),
            Err(FinalizationQueueError::EntryLimitExceeded { limit: 100 })
        );
        let stats = queue.stats();
        assert_eq!(stats.initial_capacity, 64);
        assert_eq!(stats.growth_count, 2);
        assert_eq!(stats.peak_len, 100);
        assert_eq!(queue.pop().unwrap().registry().offset(), 1);
    }
}
