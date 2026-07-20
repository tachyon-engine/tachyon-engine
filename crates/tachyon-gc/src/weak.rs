//! Typed weak-edge representations and the bounded owner worklist used by closure phases.

use core::marker::PhantomData;

use tachyon_value::{RawHeapRef, Value};

use crate::tuning::{
    CAPACITY_GROWTH_DENOMINATOR, CAPACITY_GROWTH_NUMERATOR, INITIAL_WEAK_OWNER_CAPACITY,
};
use crate::{GcRef, Trace, Tracer};

/// A nullable weak edge that never marks its target during strong traversal.
#[derive(Debug)]
pub struct WeakGcRef<T: ?Sized> {
    reference: Option<RawHeapRef>,
    marker: PhantomData<fn() -> T>,
}

impl<T: ?Sized> Copy for WeakGcRef<T> {}

impl<T: ?Sized> Clone for WeakGcRef<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> WeakGcRef<T> {
    #[must_use]
    pub const fn new(reference: GcRef<T>) -> Self {
        Self {
            reference: Some(reference.raw()),
            marker: PhantomData,
        }
    }

    #[must_use]
    pub const fn empty() -> Self {
        Self {
            reference: None,
            marker: PhantomData,
        }
    }

    #[must_use]
    pub fn get(self) -> Option<GcRef<T>> {
        self.reference.map(GcRef::from_raw)
    }
}

impl<T: ?Sized> Trace for WeakGcRef<T> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        tracer.trace_weak_raw_heap_ref(&mut self.reference);
    }
}

/// One WeakMap-style key/value pair whose value becomes strong only while its key is live.
#[derive(Clone, Copy, Debug)]
pub struct Ephemeron<K: ?Sized> {
    key: Option<RawHeapRef>,
    value: Value,
    marker: PhantomData<fn() -> K>,
}

impl<K: ?Sized> Ephemeron<K> {
    #[must_use]
    pub const fn new(key: GcRef<K>, value: Value) -> Self {
        Self {
            key: Some(key.raw()),
            value,
            marker: PhantomData,
        }
    }

    #[must_use]
    pub fn key(&self) -> Option<GcRef<K>> {
        self.key.map(GcRef::from_raw)
    }

    #[must_use]
    pub const fn value(&self) -> Value {
        self.value
    }

    /// Replaces the conditional value while retaining the weak key identity.
    pub fn replace_value(&mut self, value: Value) -> Value {
        core::mem::replace(&mut self.value, value)
    }

    /// Clears both sides before a VM-private weak table reuses the slot.
    pub fn clear(&mut self) {
        self.key = None;
        self.value = Value::from_immediate(tachyon_value::Immediate::Undefined);
    }
}

impl<K: ?Sized> Trace for Ephemeron<K> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        tracer.trace_ephemeron(&mut self.key, &mut self.value);
    }
}

/// Capacity failure before a weak owner can be retained for closure and clearing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WeakOwnerError {
    EntryLimitExceeded { limit: usize },
    AllocationFailed,
}

/// Retained high-water evidence for weak-phase capacity and leak diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WeakOwnerStats {
    pub current_len: usize,
    pub initial_capacity: usize,
    pub growth_count: usize,
    pub peak_len: usize,
    pub retained_capacity: usize,
    pub slack_entries: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WeakOwner {
    pub reference: RawHeapRef,
    pub has_weak: bool,
    pub has_ephemeron: bool,
    pub has_finalization: bool,
}

pub(crate) struct WeakOwners {
    entries: Vec<WeakOwner>,
    max_entries: usize,
    initial_capacity: usize,
    growth_count: usize,
    peak_len: usize,
}

impl WeakOwners {
    pub const fn new(max_entries: usize) -> Self {
        Self {
            entries: Vec::new(),
            max_entries,
            initial_capacity: 0,
            growth_count: 0,
            peak_len: 0,
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn get(&self, index: usize) -> Option<WeakOwner> {
        self.entries.get(index).copied()
    }

    /// Reserves by the centralized 1.5x policy before publishing one traced weak owner.
    pub fn try_push(&mut self, owner: WeakOwner) -> Result<(), WeakOwnerError> {
        if self.entries.len() == self.max_entries {
            return Err(WeakOwnerError::EntryLimitExceeded {
                limit: self.max_entries,
            });
        }
        if self.entries.len() == self.entries.capacity() {
            let target = if self.entries.capacity() == 0 {
                INITIAL_WEAK_OWNER_CAPACITY.min(self.max_entries)
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
                .map_err(|_| WeakOwnerError::AllocationFailed)?;
            if self.initial_capacity == 0 {
                self.initial_capacity = self.entries.capacity();
            } else {
                self.growth_count += 1;
            }
        }
        self.entries.push(owner);
        self.peak_len = self.peak_len.max(self.entries.len());
        Ok(())
    }

    #[must_use]
    pub fn stats(&self) -> WeakOwnerStats {
        WeakOwnerStats {
            current_len: self.entries.len(),
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
    use super::{Ephemeron, WeakOwner, WeakOwnerError, WeakOwners};
    use crate::{GcRef, RawHeapRef};
    use tachyon_value::Value;

    #[test]
    fn ephemeron_entry_updates_and_clears_without_changing_key_identity() {
        let raw = RawHeapRef::new(7).unwrap();
        let key = GcRef::<()>::from_raw(raw);
        let mut entry = Ephemeron::new(key, Value::from_i32(1));
        assert_eq!(entry.key().unwrap().raw(), raw);
        assert_eq!(entry.replace_value(Value::from_i32(2)).as_i32(), Some(1));
        assert_eq!(entry.value().as_i32(), Some(2));
        entry.clear();
        assert!(entry.key().is_none());
    }

    #[test]
    fn weak_owner_growth_and_limit_are_explicit() {
        let mut owners = WeakOwners::new(100);
        for offset in 1..=100 {
            owners
                .try_push(WeakOwner {
                    reference: RawHeapRef::new(offset).unwrap(),
                    has_weak: true,
                    has_ephemeron: false,
                    has_finalization: false,
                })
                .unwrap();
        }
        assert_eq!(
            owners.try_push(WeakOwner {
                reference: RawHeapRef::new(101).unwrap(),
                has_weak: true,
                has_ephemeron: true,
                has_finalization: true,
            }),
            Err(WeakOwnerError::EntryLimitExceeded { limit: 100 })
        );
        let stats = owners.stats();
        assert_eq!(stats.initial_capacity, 64);
        assert_eq!(stats.growth_count, 2);
        assert_eq!(stats.peak_len, 100);
        assert_eq!(owners.get(99).unwrap().reference.offset(), 100);
    }
}
