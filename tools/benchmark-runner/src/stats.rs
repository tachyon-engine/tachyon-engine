use serde::{Deserialize, Serialize};

/// Robust timing summary in integer nanoseconds with an explicit confidence approximation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SampleSummary {
    /// Raw sample count.
    pub collected: usize,
    /// Samples remaining after the fixed outlier rule.
    pub retained: usize,
    /// Samples rejected by the MAD cutoff.
    pub rejected_outliers: usize,
    /// Retained median duration.
    pub median_ns: u64,
    /// Retained median absolute deviation.
    pub mad_ns: u64,
    /// `mad_ns / median_ns` noise ratio.
    pub relative_mad: f64,
    /// Lower approximate 95% confidence bound.
    pub confidence_low_ns: u64,
    /// Upper approximate 95% confidence bound.
    pub confidence_high_ns: u64,
    /// Human-readable statistical method identifier.
    pub confidence_method: Box<str>,
}

/// Samples cannot produce a release-quality robust summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StatisticsError {
    /// Raw sample count is below the configured minimum.
    TooFewCollected {
        /// Required samples.
        minimum: usize,
        /// Observed samples.
        actual: usize,
    },
    /// Outlier rejection left too few samples.
    TooFewRetained {
        /// Required retained samples.
        minimum: usize,
        /// Retained samples.
        actual: usize,
    },
    /// Timer produced no measurable nonzero center.
    ZeroMedian,
    /// Element count cannot be represented as an allocation size.
    CapacityOverflow,
    /// Fallible sample/deviation allocation failed.
    AllocationFailed,
}

/// Applies a fixed MAD outlier rule, then reports median/MAD and a normal robust-standard-error interval.
pub fn summarize_samples(
    samples: &[u64],
    minimum_samples: usize,
    outlier_mad_multiplier: f64,
) -> Result<SampleSummary, StatisticsError> {
    if samples.len() < minimum_samples {
        return Err(StatisticsError::TooFewCollected {
            minimum: minimum_samples,
            actual: samples.len(),
        });
    }
    let mut sorted = try_copy(samples)?;
    sorted.sort_unstable();
    let initial_median = median(&sorted);
    if initial_median == 0 {
        return Err(StatisticsError::ZeroMedian);
    }
    let mut deviations = Vec::new();
    deviations
        .try_reserve_exact(sorted.len())
        .map_err(|_| StatisticsError::AllocationFailed)?;
    deviations.extend(sorted.iter().map(|sample| sample.abs_diff(initial_median)));
    deviations.sort_unstable();
    let initial_mad = median(&deviations);
    let threshold = (initial_mad as f64 * outlier_mad_multiplier).ceil() as u64;
    let retained = if initial_mad == 0 {
        sorted
    } else {
        sorted
            .into_iter()
            .filter(|sample| sample.abs_diff(initial_median) <= threshold)
            .collect()
    };
    if retained.len() < minimum_samples {
        return Err(StatisticsError::TooFewRetained {
            minimum: minimum_samples,
            actual: retained.len(),
        });
    }
    let median_ns = median(&retained);
    if median_ns == 0 {
        return Err(StatisticsError::ZeroMedian);
    }
    let mut final_deviations = Vec::new();
    final_deviations
        .try_reserve_exact(retained.len())
        .map_err(|_| StatisticsError::AllocationFailed)?;
    final_deviations.extend(retained.iter().map(|sample| sample.abs_diff(median_ns)));
    final_deviations.sort_unstable();
    let mad_ns = median(&final_deviations);
    let robust_standard_error = 1.4826 * mad_ns as f64 / (retained.len() as f64).sqrt();
    let margin = 1.96 * robust_standard_error;
    Ok(SampleSummary {
        collected: samples.len(),
        retained: retained.len(),
        rejected_outliers: samples.len() - retained.len(),
        median_ns,
        mad_ns,
        relative_mad: mad_ns as f64 / median_ns as f64,
        confidence_low_ns: (median_ns as f64 - margin).max(0.0).floor() as u64,
        confidence_high_ns: (median_ns as f64 + margin).ceil() as u64,
        confidence_method: "95% normal interval from MAD-based robust standard error".into(),
    })
}

fn try_copy(samples: &[u64]) -> Result<Vec<u64>, StatisticsError> {
    let bytes = samples
        .len()
        .checked_mul(core::mem::size_of::<u64>())
        .ok_or(StatisticsError::CapacityOverflow)?;
    let _ = bytes;
    let mut copied = Vec::new();
    copied
        .try_reserve_exact(samples.len())
        .map_err(|_| StatisticsError::AllocationFailed)?;
    copied.extend_from_slice(samples);
    Ok(copied)
}

fn median(sorted: &[u64]) -> u64 {
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        sorted[middle - 1] / 2
            + sorted[middle] / 2
            + ((sorted[middle - 1] % 2 + sorted[middle] % 2) / 2)
    } else {
        sorted[middle]
    }
}

impl core::fmt::Display for StatisticsError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "invalid benchmark samples: {self:?}")
    }
}

impl std::error::Error for StatisticsError {}

#[cfg(test)]
mod tests {
    use super::{StatisticsError, summarize_samples};

    #[test]
    fn summary_rejects_one_large_outlier_and_keeps_robust_center() {
        let summary = summarize_samples(
            &[99, 100, 100, 100, 101, 101, 99, 100, 102, 98, 100, 50_000],
            10,
            6.0,
        )
        .unwrap();
        assert_eq!(summary.median_ns, 100);
        assert_eq!(summary.rejected_outliers, 1);
        assert!(summary.confidence_low_ns <= 100);
        assert!(summary.confidence_high_ns >= 100);
    }

    #[test]
    fn summary_requires_ten_nonzero_retained_samples() {
        assert_eq!(
            summarize_samples(&[1; 9], 10, 6.0),
            Err(StatisticsError::TooFewCollected {
                minimum: 10,
                actual: 9,
            })
        );
        assert_eq!(
            summarize_samples(&[0; 10], 10, 6.0),
            Err(StatisticsError::ZeroMedian)
        );
    }
}
