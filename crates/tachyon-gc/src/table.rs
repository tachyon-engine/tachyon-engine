//! Stable logical span indexing over independently allocated native storage.

use crate::{
    MAX_LOGICAL_SPANS, SizeClass, SmallSpanMetadata, SpanId, SpanReuseGeneration, SpanSpace,
    SpanStorage, SpanStorageAllocationError,
    tuning::{
        CAPACITY_GROWTH_DENOMINATOR, CAPACITY_GROWTH_NUMERATOR, INITIAL_FREE_RANGE_CAPACITY,
        INITIAL_SPAN_TABLE_CAPACITY,
    },
};

const MAX_FREE_SPAN_RANGES: usize = MAX_LOGICAL_SPANS.div_ceil(2);

/// A recoverable failure at the span-table allocation or validation boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpanTableError {
    AddressSpaceExhausted,
    TableAllocationFailed,
    FreeRangeAllocationFailed,
    StorageAllocationFailed,
    UnknownSpan(SpanId),
    VacantSpan(SpanId),
}

impl From<SpanStorageAllocationError> for SpanTableError {
    fn from(_: SpanStorageAllocationError) -> Self {
        Self::StorageAllocationFailed
    }
}

struct SmallSpan {
    storage: SpanStorage,
    metadata: SmallSpanMetadata,
}

struct SpanEntry {
    generation: SpanReuseGeneration,
    span: Option<SmallSpan>,
}

/// A coalesced inclusive range of vacant logical span IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FreeSpanRange {
    start: u16,
    end: u16,
}

/// An isolate-local mapping whose entry indices remain stable across metadata-vector growth.
#[derive(Default)]
pub struct SpanTable {
    entries: Vec<SpanEntry>,
    free_ranges: Vec<FreeSpanRange>,
    live_spans: usize,
}

impl SpanTable {
    /// Creates an empty table; the first slow-path insertion performs the educated reservation.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            free_ranges: Vec::new(),
            live_spans: 0,
        }
    }

    /// Allocates one small-object span and installs it at a reused or new stable table index.
    pub fn try_allocate_small(
        &mut self,
        size_class: SizeClass,
        space: SpanSpace,
    ) -> Result<SpanId, SpanTableError> {
        let storage = SpanStorage::try_new()?;
        if let Some(span_id) = self.take_free_span() {
            self.install_reused(span_id, storage, size_class, space);
            return Ok(span_id);
        }

        self.install_new(storage, size_class, space)
    }

    /// Releases native storage while retaining the table index and its incremented reuse generation.
    pub fn release(&mut self, span_id: SpanId) -> Result<(), SpanTableError> {
        let index = span_id.index() as usize;
        let Some(entry) = self.entries.get(index) else {
            return Err(SpanTableError::UnknownSpan(span_id));
        };
        if entry.span.is_none() {
            return Err(SpanTableError::VacantSpan(span_id));
        }
        self.reserve_free_range_if_needed(span_id.index())?;

        let entry = &mut self.entries[index];
        entry.span = None;
        entry.generation = entry.generation.next();
        self.insert_free_span(span_id.index());
        self.live_spans -= 1;
        Ok(())
    }

    /// Resolves through the current table entry every time instead of caching a native object pointer.
    #[must_use]
    pub fn base_address(&self, span_id: SpanId) -> Option<*const u8> {
        self.entries
            .get(span_id.index() as usize)?
            .span
            .as_ref()
            .map(|span| span.storage.base_address())
    }

    /// Returns immutable side metadata for collector and verifier operations.
    #[must_use]
    pub fn metadata(&self, span_id: SpanId) -> Option<&SmallSpanMetadata> {
        self.entries
            .get(span_id.index() as usize)?
            .span
            .as_ref()
            .map(|span| &span.metadata)
    }

    /// Returns mutable side metadata while preserving exclusive isolate ownership.
    #[must_use]
    pub fn metadata_mut(&mut self, span_id: SpanId) -> Option<&mut SmallSpanMetadata> {
        self.entries
            .get_mut(span_id.index() as usize)?
            .span
            .as_mut()
            .map(|span| &mut span.metadata)
    }

    /// Returns the count of currently backed entries.
    #[must_use]
    pub const fn live_spans(&self) -> usize {
        self.live_spans
    }

    /// Returns the historical table length; released trailing IDs deliberately do not shrink it.
    #[must_use]
    pub fn historical_span_count(&self) -> usize {
        self.entries.len()
    }

    /// Returns retained entry capacity for later capacity instrumentation and accounting.
    #[must_use]
    pub fn retained_entry_capacity(&self) -> usize {
        self.entries.capacity()
    }

    fn install_new(
        &mut self,
        storage: SpanStorage,
        size_class: SizeClass,
        space: SpanSpace,
    ) -> Result<SpanId, SpanTableError> {
        if self.entries.len() == MAX_LOGICAL_SPANS {
            return Err(SpanTableError::AddressSpaceExhausted);
        }
        reserve_for_push(
            &mut self.entries,
            INITIAL_SPAN_TABLE_CAPACITY,
            MAX_LOGICAL_SPANS,
            SpanTableError::TableAllocationFailed,
        )?;
        let index = u16::try_from(self.entries.len()).expect("logical span limit checked above");
        let generation = SpanReuseGeneration::INITIAL;
        self.entries.push(SpanEntry {
            generation,
            span: Some(SmallSpan {
                storage,
                metadata: SmallSpanMetadata::new(size_class, space, generation),
            }),
        });
        self.live_spans += 1;
        Ok(SpanId::new(index))
    }

    fn install_reused(
        &mut self,
        span_id: SpanId,
        storage: SpanStorage,
        size_class: SizeClass,
        space: SpanSpace,
    ) {
        let entry = &mut self.entries[span_id.index() as usize];
        debug_assert!(entry.span.is_none());
        entry.span = Some(SmallSpan {
            storage,
            metadata: SmallSpanMetadata::new(size_class, space, entry.generation),
        });
        self.live_spans += 1;
    }

    fn take_free_span(&mut self) -> Option<SpanId> {
        let range = self.free_ranges.last_mut()?;
        let index = range.end;
        if range.start == range.end {
            self.free_ranges.pop();
        } else {
            range.end -= 1;
        }
        Some(SpanId::new(index))
    }

    fn reserve_free_range_if_needed(&mut self, index: u16) -> Result<(), SpanTableError> {
        if self.adjacent_free_range(index).is_some() {
            return Ok(());
        }
        reserve_for_push(
            &mut self.free_ranges,
            INITIAL_FREE_RANGE_CAPACITY,
            MAX_FREE_SPAN_RANGES,
            SpanTableError::FreeRangeAllocationFailed,
        )
    }

    fn adjacent_free_range(&self, index: u16) -> Option<usize> {
        self.free_ranges.iter().position(|range| {
            range.start as u32 <= index as u32 + 1 && index as u32 <= range.end as u32 + 1
        })
    }

    /// Inserts one newly vacant ID and coalesces both neighboring ranges without allocating.
    fn insert_free_span(&mut self, index: u16) {
        let position = self
            .free_ranges
            .partition_point(|range| range.start < index);
        let joins_left =
            position > 0 && self.free_ranges[position - 1].end as u32 + 1 == index as u32;
        let joins_right = position < self.free_ranges.len()
            && index as u32 + 1 == self.free_ranges[position].start as u32;

        match (joins_left, joins_right) {
            (true, true) => {
                let right_end = self.free_ranges[position].end;
                self.free_ranges[position - 1].end = right_end;
                self.free_ranges.remove(position);
            }
            (true, false) => self.free_ranges[position - 1].end = index,
            (false, true) => self.free_ranges[position].start = index,
            (false, false) => self.free_ranges.insert(
                position,
                FreeSpanRange {
                    start: index,
                    end: index,
                },
            ),
        }
    }
}

/// Reserves a centralized 1.5x capacity step before a push can mutate container state.
fn reserve_for_push<T>(
    values: &mut Vec<T>,
    initial_capacity: usize,
    maximum_capacity: usize,
    error: SpanTableError,
) -> Result<(), SpanTableError> {
    if values.len() < values.capacity() {
        return Ok(());
    }
    let target = if values.capacity() == 0 {
        initial_capacity
    } else {
        values
            .capacity()
            .saturating_mul(CAPACITY_GROWTH_NUMERATOR)
            .div_ceil(CAPACITY_GROWTH_DENOMINATOR)
    };
    let target = target.max(values.len() + 1).min(maximum_capacity);
    let additional = target - values.len();
    values.try_reserve_exact(additional).map_err(|_| error)
}

#[cfg(test)]
mod tests {
    use super::{SpanTable, SpanTableError};
    use crate::{SizeClass, SpanId, SpanReuseGeneration, SpanSpace};

    fn size_class() -> SizeClass {
        SizeClass::new(16).expect("minimum size class")
    }

    #[test]
    /// Forces metadata-vector growth and proves independently allocated span storage stays stable.
    fn table_grows_on_demand_and_resolves_stable_independent_storage() {
        let mut table = SpanTable::new();
        assert_eq!(table.historical_span_count(), 0);
        assert_eq!(table.retained_entry_capacity(), 0);

        let first = table
            .try_allocate_small(size_class(), SpanSpace::Eden)
            .unwrap();
        let first_address = table.base_address(first).unwrap();
        let mut last = first;
        for _ in 1..32 {
            last = table
                .try_allocate_small(size_class(), SpanSpace::Old)
                .unwrap();
        }

        assert_eq!(first, SpanId::new(0));
        assert_eq!(last, SpanId::new(31));
        assert_eq!(table.live_spans(), 32);
        assert_eq!(table.historical_span_count(), 32);
        assert_eq!(table.base_address(first), Some(first_address));
        assert_ne!(table.base_address(last), Some(first_address));
    }

    #[test]
    /// Releases coalesced ranges, reuses a stable ID, and exposes its incremented generation.
    fn table_reuses_free_ranges_without_shrinking_historical_indices() {
        let mut table = SpanTable::new();
        let first = table
            .try_allocate_small(size_class(), SpanSpace::Eden)
            .unwrap();
        let second = table
            .try_allocate_small(size_class(), SpanSpace::Eden)
            .unwrap();
        let third = table
            .try_allocate_small(size_class(), SpanSpace::Eden)
            .unwrap();

        table.release(first).unwrap();
        table.release(second).unwrap();
        table.release(third).unwrap();
        assert_eq!(table.live_spans(), 0);
        assert_eq!(table.historical_span_count(), 3);
        assert_eq!(table.base_address(second), None);
        assert_eq!(
            table.release(second),
            Err(SpanTableError::VacantSpan(second))
        );

        let reused = table
            .try_allocate_small(size_class(), SpanSpace::Old)
            .unwrap();
        assert_eq!(reused, third);
        assert_eq!(
            table.metadata(reused).unwrap().reuse_generation(),
            SpanReuseGeneration::INITIAL.next()
        );
        assert_eq!(table.historical_span_count(), 3);
    }

    #[test]
    fn unknown_span_ids_are_rejected_without_mutating_the_table() {
        let mut table = SpanTable::new();
        let unknown = SpanId::new(7);
        assert_eq!(
            table.release(unknown),
            Err(SpanTableError::UnknownSpan(unknown))
        );
        assert_eq!(table.live_spans(), 0);
    }
}
