//! Fixed-capacity GC pause aggregation fed by a host-provided monotonic clock boundary.

use core::time::Duration;

/// Zero, 64 power-of-two nanosecond bounds, and one saturated overflow bucket.
const PAUSE_HISTOGRAM_BUCKETS: usize = 66;

/// Selects the independently aggregated stop-the-world collection phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionKind {
    Minor,
    Major,
}

/// Approximate percentile upper bounds and exact total/max evidence for one collection kind.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PauseHistogramStats {
    pub samples: u64,
    pub total_nanos: u128,
    pub max_nanos: u64,
    pub p50_upper_nanos: Option<u64>,
    pub p95_upper_nanos: Option<u64>,
    pub p99_upper_nanos: Option<u64>,
}

/// Minor and major distributions never merge unlike phases with different latency budgets.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GcPauseStats {
    pub minor: PauseHistogramStats,
    pub major: PauseHistogramStats,
}

pub(crate) struct PauseHistogram {
    buckets: [u64; PAUSE_HISTOGRAM_BUCKETS],
    samples: u64,
    total_nanos: u128,
    max_nanos: u64,
}

impl PauseHistogram {
    pub const fn new() -> Self {
        Self {
            buckets: [0; PAUSE_HISTOGRAM_BUCKETS],
            samples: 0,
            total_nanos: 0,
            max_nanos: 0,
        }
    }

    /// Records caller-measured elapsed time without consulting a clock or allocating a sample.
    pub fn record(&mut self, elapsed: Duration) {
        let nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        let bucket = bucket_index(nanos);
        self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
        self.samples = self.samples.saturating_add(1);
        self.total_nanos = self.total_nanos.saturating_add(u128::from(nanos));
        self.max_nanos = self.max_nanos.max(nanos);
    }

    /// Resolves percentile ranks against fixed bucket upper bounds in one bounded pass.
    pub fn stats(&self) -> PauseHistogramStats {
        PauseHistogramStats {
            samples: self.samples,
            total_nanos: self.total_nanos,
            max_nanos: self.max_nanos,
            p50_upper_nanos: self.percentile_upper(50),
            p95_upper_nanos: self.percentile_upper(95),
            p99_upper_nanos: self.percentile_upper(99),
        }
    }

    fn percentile_upper(&self, percentile: u8) -> Option<u64> {
        if self.samples == 0 {
            return None;
        }
        let rank = (u128::from(self.samples) * u128::from(percentile)).div_ceil(100);
        let mut cumulative = 0_u128;
        for (index, count) in self.buckets.iter().copied().enumerate() {
            cumulative += u128::from(count);
            if cumulative >= rank {
                return Some(bucket_upper_bound(index));
            }
        }
        Some(u64::MAX)
    }
}

pub(crate) struct GcPauses {
    minor: PauseHistogram,
    major: PauseHistogram,
}

impl GcPauses {
    pub const fn new() -> Self {
        Self {
            minor: PauseHistogram::new(),
            major: PauseHistogram::new(),
        }
    }

    pub fn record(&mut self, kind: CollectionKind, elapsed: Duration) {
        match kind {
            CollectionKind::Minor => self.minor.record(elapsed),
            CollectionKind::Major => self.major.record(elapsed),
        }
    }

    pub fn stats(&self) -> GcPauseStats {
        GcPauseStats {
            minor: self.minor.stats(),
            major: self.major.stats(),
        }
    }
}

/// Maps zero exactly and positive nanoseconds to `ceil(log2(nanos))` upper-bound buckets.
#[inline(always)]
fn bucket_index(nanos: u64) -> usize {
    if nanos == 0 {
        return 0;
    }
    let ceil_log2 = u64::BITS - nanos.saturating_sub(1).leading_zeros();
    (ceil_log2 as usize + 1).min(PAUSE_HISTOGRAM_BUCKETS - 1)
}

#[inline(always)]
fn bucket_upper_bound(index: usize) -> u64 {
    match index {
        0 => 0,
        1..=64 => 1_u64 << (index - 1),
        _ => u64::MAX,
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::{CollectionKind, GcPauses, PauseHistogram};

    #[test]
    fn log_buckets_report_rank_upper_bounds_and_exact_total_max() {
        let mut histogram = PauseHistogram::new();
        for nanos in [1, 2, 3, 4, 8] {
            histogram.record(Duration::from_nanos(nanos));
        }

        let stats = histogram.stats();
        assert_eq!(stats.samples, 5);
        assert_eq!(stats.total_nanos, 18);
        assert_eq!(stats.max_nanos, 8);
        assert_eq!(stats.p50_upper_nanos, Some(4));
        assert_eq!(stats.p95_upper_nanos, Some(8));
        assert_eq!(stats.p99_upper_nanos, Some(8));
    }

    #[test]
    fn minor_and_major_histograms_are_independent_and_zero_is_exact() {
        let mut pauses = GcPauses::new();
        pauses.record(CollectionKind::Minor, Duration::ZERO);
        pauses.record(CollectionKind::Major, Duration::from_nanos(9));

        let stats = pauses.stats();
        assert_eq!(stats.minor.p50_upper_nanos, Some(0));
        assert_eq!(stats.minor.max_nanos, 0);
        assert_eq!(stats.major.p50_upper_nanos, Some(16));
        assert_eq!(stats.major.max_nanos, 9);
    }
}
