//! Fixed-capacity per-size-class retention for empty Eden span backing.

use crate::{
    SPAN_SIZE_BYTES, SpanId,
    tuning::{EDEN_POOL_SPANS_PER_SIZE_CLASS, SMALL_SIZE_CLASSES},
};

/// Current retention and cumulative pool transition evidence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EdenPoolStats {
    pub retained_spans: usize,
    pub retained_bytes: usize,
    pub spans_pooled: usize,
    pub spans_reused: usize,
    pub spans_trimmed: usize,
    pub overflow_releases: usize,
}

pub(crate) struct EdenPool {
    spans: [[Option<SpanId>; EDEN_POOL_SPANS_PER_SIZE_CLASS]; SMALL_SIZE_CLASSES.len()],
    stats: EdenPoolStats,
}

impl EdenPool {
    /// Creates all per-class slots inline without allocator calls or deferred capacity growth.
    pub const fn new() -> Self {
        Self {
            spans: [[None; EDEN_POOL_SPANS_PER_SIZE_CLASS]; SMALL_SIZE_CLASSES.len()],
            stats: EdenPoolStats {
                retained_spans: 0,
                retained_bytes: 0,
                spans_pooled: 0,
                spans_reused: 0,
                spans_trimmed: 0,
                overflow_releases: 0,
            },
        }
    }

    #[must_use]
    pub const fn has_retained(&self, class_index: usize) -> bool {
        let mut pool_index = 0;
        while pool_index < EDEN_POOL_SPANS_PER_SIZE_CLASS {
            if self.spans[class_index][pool_index].is_some() {
                return true;
            }
            pool_index += 1;
        }
        false
    }

    #[must_use]
    pub const fn has_capacity(&self, class_index: usize) -> bool {
        let mut pool_index = 0;
        while pool_index < EDEN_POOL_SPANS_PER_SIZE_CLASS {
            if self.spans[class_index][pool_index].is_none() {
                return true;
            }
            pool_index += 1;
        }
        false
    }

    /// Retains one span if its size-class slot is empty; callers release overflow immediately.
    pub fn retain(&mut self, class_index: usize, span_id: SpanId) -> bool {
        let Some(pool_index) = self.spans[class_index].iter().position(Option::is_none) else {
            self.stats.overflow_releases = self.stats.overflow_releases.saturating_add(1);
            return false;
        };
        self.spans[class_index][pool_index] = Some(span_id);
        self.stats.retained_spans += 1;
        self.stats.retained_bytes += SPAN_SIZE_BYTES;
        self.stats.spans_pooled = self.stats.spans_pooled.saturating_add(1);
        true
    }

    /// Removes one matching retained span for allocation without scanning other size classes.
    pub fn take_for_reuse(&mut self, class_index: usize) -> Option<SpanId> {
        let pool_index = self.spans[class_index].iter().position(Option::is_some)?;
        let span_id = self.spans[class_index][pool_index].take()?;
        self.stats.retained_spans -= 1;
        self.stats.retained_bytes -= SPAN_SIZE_BYTES;
        self.stats.spans_reused = self.stats.spans_reused.saturating_add(1);
        Some(span_id)
    }

    #[must_use]
    pub fn first_retained(&self, class_index: usize) -> Option<(usize, SpanId)> {
        self.spans[class_index]
            .iter()
            .enumerate()
            .find_map(|(index, span)| span.map(|span| (index, span)))
    }

    /// Removes a span only after the table has successfully released its backing storage.
    pub fn record_trimmed(&mut self, class_index: usize, pool_index: usize, expected: SpanId) {
        let retained = self.spans[class_index][pool_index]
            .take()
            .expect("trim only removes a retained pool span");
        assert_eq!(retained, expected, "trimmed span must match the pool slot");
        self.stats.retained_spans -= 1;
        self.stats.retained_bytes -= SPAN_SIZE_BYTES;
        self.stats.spans_trimmed = self.stats.spans_trimmed.saturating_add(1);
    }

    #[must_use]
    pub const fn stats(&self) -> EdenPoolStats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::EdenPool;
    use crate::{SPAN_SIZE_BYTES, SpanId};

    #[test]
    fn one_slot_per_size_class_has_exact_retain_reuse_and_overflow_accounting() {
        let mut pool = EdenPool::new();
        assert!(pool.has_capacity(0));
        assert!(pool.retain(0, SpanId::new(1)));
        assert!(!pool.has_capacity(0));
        assert!(!pool.retain(0, SpanId::new(2)));
        assert_eq!(pool.take_for_reuse(0), Some(SpanId::new(1)));

        let stats = pool.stats();
        assert_eq!(stats.retained_spans, 0);
        assert_eq!(stats.retained_bytes, 0);
        assert_eq!(stats.spans_pooled, 1);
        assert_eq!(stats.spans_reused, 1);
        assert_eq!(stats.overflow_releases, 1);
        assert_eq!(SPAN_SIZE_BYTES, 64 * 1024);
    }
}
