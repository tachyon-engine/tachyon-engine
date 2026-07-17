use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::BENCHMARK_REPORT_SCHEMA_VERSION;
use crate::{BenchmarkCaseResult, BenchmarkCategory, BenchmarkReport, MeasurementMode, SuiteKind};

/// Stable identity shared by baseline and candidate engines.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CaseKey {
    /// Approved script ID.
    pub script_id: Box<str>,
    /// Exact timing boundary.
    pub mode: MeasurementMode,
}

/// One candidate speed ratio; values above one mean candidate is faster.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CaseRatio {
    /// Stable script/mode identity.
    pub key: CaseKey,
    /// Subsystem aggregate category.
    pub category: BenchmarkCategory,
    /// Corpus aggregate suite.
    pub suite: SuiteKind,
    /// Baseline retained median nanoseconds.
    pub baseline_median_ns: u64,
    /// Candidate retained median nanoseconds.
    pub candidate_median_ns: u64,
    /// Baseline median normalized by explicit executions per sample.
    pub baseline_ns_per_iteration: f64,
    /// Candidate median normalized by explicit executions per sample.
    pub candidate_ns_per_iteration: f64,
    /// `baseline / candidate`; above one is faster.
    pub speed_ratio: f64,
}

/// Fair matched-case comparison and geometric means.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkComparison {
    /// Machine-readable comparison contract.
    pub schema_version: u32,
    /// Baseline engine name.
    pub baseline_engine: Box<str>,
    /// Candidate engine name.
    pub candidate_engine: Box<str>,
    /// Whether all keys exist and all matched cases passed validity gates.
    pub valid: bool,
    /// Matched valid per-case ratios.
    pub cases: Vec<CaseRatio>,
    /// Candidate-missing baseline keys.
    pub missing_candidate: Vec<CaseKey>,
    /// Baseline-missing candidate keys.
    pub missing_baseline: Vec<CaseKey>,
    /// Matched keys excluded because either case was invalid.
    pub invalid_cases: Vec<CaseKey>,
    /// Geometric mean across all valid matched cases.
    pub geometric_mean: Option<f64>,
    /// Geometric means split by subsystem category.
    pub by_category: BTreeMap<BenchmarkCategory, f64>,
    /// Geometric means split by corpus suite.
    pub by_suite: BTreeMap<SuiteKind, f64>,
}

/// Reports cannot form a fair comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompareError {
    /// Result schemas differ.
    SchemaMismatch {
        /// Baseline schema.
        baseline: u32,
        /// Candidate schema.
        candidate: u32,
    },
    /// Host identity differs.
    HostMismatch,
    /// One report contains a duplicate script/mode key.
    DuplicateKey(CaseKey),
    /// Matched keys contain different source bytes.
    ScriptHashMismatch(CaseKey),
    /// Matched keys disagree about their aggregate category or suite.
    ClassificationMismatch(CaseKey),
    /// Matched keys represent different execution counts per sample.
    IterationCountMismatch(CaseKey),
}

/// Matches script/mode keys, excludes invalid cases, and computes explicit baseline/candidate ratios.
pub fn compare_reports(
    baseline: &BenchmarkReport,
    candidate: &BenchmarkReport,
) -> Result<BenchmarkComparison, CompareError> {
    if baseline.schema_version != candidate.schema_version {
        return Err(CompareError::SchemaMismatch {
            baseline: baseline.schema_version,
            candidate: candidate.schema_version,
        });
    }
    if !baseline.host.same_static_identity(&candidate.host) {
        return Err(CompareError::HostMismatch);
    }
    let mut baseline_cases = index_cases(&baseline.cases)?;
    let candidate_cases = index_cases(&candidate.cases)?;
    let mut ratios = Vec::new();
    let mut missing_baseline = Vec::new();
    let mut invalid_cases = Vec::new();
    for (key, candidate_case) in candidate_cases {
        let Some(baseline_case) = baseline_cases.remove(&key) else {
            missing_baseline.push(key);
            continue;
        };
        if baseline_case.script_sha256 != candidate_case.script_sha256 {
            return Err(CompareError::ScriptHashMismatch(key));
        }
        if baseline_case.category != candidate_case.category
            || baseline_case.suite != candidate_case.suite
        {
            return Err(CompareError::ClassificationMismatch(key));
        }
        if baseline_case.iterations_per_sample != candidate_case.iterations_per_sample {
            return Err(CompareError::IterationCountMismatch(key));
        }
        if !baseline_case.validity.valid || !candidate_case.validity.valid {
            invalid_cases.push(key);
            continue;
        }
        let baseline_ns_per_iteration =
            baseline_case.summary.median_ns as f64 / baseline_case.iterations_per_sample as f64;
        let candidate_ns_per_iteration =
            candidate_case.summary.median_ns as f64 / candidate_case.iterations_per_sample as f64;
        ratios.push(CaseRatio {
            key,
            category: candidate_case.category,
            suite: candidate_case.suite,
            baseline_median_ns: baseline_case.summary.median_ns,
            candidate_median_ns: candidate_case.summary.median_ns,
            baseline_ns_per_iteration,
            candidate_ns_per_iteration,
            speed_ratio: baseline_ns_per_iteration / candidate_ns_per_iteration,
        });
    }
    let missing_candidate = baseline_cases.into_keys().collect::<Vec<_>>();
    let by_category = grouped_geomean(&ratios, |ratio| ratio.category);
    let by_suite = grouped_geomean(&ratios, |ratio| ratio.suite);
    Ok(BenchmarkComparison {
        schema_version: BENCHMARK_REPORT_SCHEMA_VERSION,
        baseline_engine: report_engine_name(baseline),
        candidate_engine: report_engine_name(candidate),
        valid: missing_candidate.is_empty()
            && missing_baseline.is_empty()
            && invalid_cases.is_empty()
            && !ratios.is_empty(),
        geometric_mean: geometric_mean(ratios.iter().map(|ratio| ratio.speed_ratio)),
        cases: ratios,
        missing_candidate,
        missing_baseline,
        invalid_cases,
        by_category,
        by_suite,
    })
}

impl BenchmarkComparison {
    /// Emits a compact summary; JSON remains the source for every per-case ratio.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        format!(
            "# Benchmark comparison\n\n| Metric | Value |\n| --- | ---: |\n| Valid | {} |\n| Matched cases | {} |\n| Missing candidate | {} |\n| Missing baseline | {} |\n| Invalid cases | {} |\n| Geometric mean | {} |\n",
            self.valid,
            self.cases.len(),
            self.missing_candidate.len(),
            self.missing_baseline.len(),
            self.invalid_cases.len(),
            self.geometric_mean
                .map_or_else(|| "n/a".to_owned(), |value| format!("{value:.6}"))
        )
    }
}

fn index_cases(
    cases: &[BenchmarkCaseResult],
) -> Result<BTreeMap<CaseKey, &BenchmarkCaseResult>, CompareError> {
    let mut indexed = BTreeMap::new();
    for case in cases {
        let key = CaseKey {
            script_id: case.script_id.clone(),
            mode: case.mode,
        };
        if indexed.insert(key.clone(), case).is_some() {
            return Err(CompareError::DuplicateKey(key));
        }
    }
    Ok(indexed)
}

fn report_engine_name(report: &BenchmarkReport) -> Box<str> {
    report
        .cases
        .first()
        .map_or_else(|| "unknown".into(), |case| case.engine.name.clone())
}

fn grouped_geomean<K: Copy + Ord>(
    ratios: &[CaseRatio],
    key: impl Fn(&CaseRatio) -> K,
) -> BTreeMap<K, f64> {
    let mut groups: BTreeMap<K, Vec<f64>> = BTreeMap::new();
    for ratio in ratios {
        groups
            .entry(key(ratio))
            .or_default()
            .push(ratio.speed_ratio);
    }
    groups
        .into_iter()
        .filter_map(|(key, values)| geometric_mean(values).map(|mean| (key, mean)))
        .collect()
}

fn geometric_mean(values: impl IntoIterator<Item = f64>) -> Option<f64> {
    let mut log_sum = 0.0;
    let mut count = 0_u64;
    for value in values {
        if !value.is_finite() || value <= 0.0 {
            return None;
        }
        log_sum += value.ln();
        count += 1;
    }
    (count != 0).then(|| (log_sum / count as f64).exp())
}

impl core::fmt::Display for CompareError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "benchmark comparison failed: {self:?}")
    }
}

impl std::error::Error for CompareError {}

#[cfg(test)]
mod tests {
    use crate::{
        BENCHMARK_REPORT_SCHEMA_VERSION, BenchmarkCaseResult, BenchmarkCategory, BenchmarkReport,
        BuildConfig, EngineIdentity, EngineKind, HostMetadata, MeasurementMode, SampleSummary,
        SuiteKind, Validity,
    };

    use super::{CompareError, compare_reports};

    fn case(id: &str, median: u64) -> BenchmarkCaseResult {
        BenchmarkCaseResult {
            script_id: id.into(),
            script_sha256: "hash".into(),
            category: BenchmarkCategory::Dispatch,
            suite: SuiteKind::Micro,
            mode: MeasurementMode::SteadyState,
            engine: EngineIdentity {
                name: "engine".into(),
                kind: EngineKind::Fixture,
                version: "1".into(),
                commit: "fixture".into(),
                features: "none".into(),
                build_flags: "fixture".into(),
                binary_size_bytes: None,
            },
            samples_ns: vec![median; 10],
            iterations_per_sample: 1,
            peak_rss_bytes: None,
            summary: SampleSummary {
                collected: 10,
                retained: 10,
                rejected_outliers: 0,
                median_ns: median,
                mad_ns: 0,
                relative_mad: 0.0,
                confidence_low_ns: median,
                confidence_high_ns: median,
                confidence_method: "fixture".into(),
            },
            validity: Validity {
                valid: true,
                reasons: Vec::new(),
            },
        }
    }

    fn report(cases: Vec<BenchmarkCaseResult>) -> BenchmarkReport {
        BenchmarkReport {
            schema_version: BENCHMARK_REPORT_SCHEMA_VERSION,
            host: HostMetadata {
                os: "test".into(),
                architecture: "test".into(),
                cpu: "test".into(),
                rustc: "test".into(),
                cpu_affinity: crate::runner::EnvironmentCheck {
                    satisfied: true,
                    detail: "fixture".into(),
                },
                performance_governor: crate::runner::EnvironmentCheck {
                    satisfied: true,
                    detail: "fixture".into(),
                },
                background_noise: None,
                validity: Validity {
                    valid: true,
                    reasons: Vec::new(),
                },
            },
            build: BuildConfig {
                profile: "release".into(),
                panic: "unwind".into(),
                lto: "thin".into(),
                codegen_units: 1,
                target_cpu: "default".into(),
                features: "default".into(),
            },
            cases,
        }
    }

    #[test]
    fn comparison_reports_case_and_geometric_mean_speed_ratios() {
        let baseline = report(vec![case("a", 200), case("b", 800)]);
        let candidate = report(vec![case("a", 100), case("b", 200)]);
        let comparison = compare_reports(&baseline, &candidate).unwrap();
        assert!(comparison.valid);
        assert_eq!(comparison.cases[0].speed_ratio, 2.0);
        assert_eq!(comparison.cases[0].baseline_ns_per_iteration, 200.0);
        assert_eq!(comparison.cases[0].candidate_ns_per_iteration, 100.0);
        assert_eq!(comparison.cases[1].speed_ratio, 4.0);
        assert!((comparison.geometric_mean.unwrap() - 8.0_f64.sqrt()).abs() < 1e-12);
        assert!(comparison.to_markdown().contains("2.828427"));
    }

    #[test]
    fn comparison_ignores_dynamic_host_probe_differences() {
        let baseline = report(vec![case("a", 200)]);
        let mut candidate = report(vec![case("a", 100)]);
        candidate.host.cpu_affinity.satisfied = false;
        candidate.host.validity.valid = false;
        candidate.host.validity.reasons.push("fixture noise".into());
        assert!(compare_reports(&baseline, &candidate).unwrap().valid);
    }

    #[test]
    fn comparison_excludes_invalid_cases_and_marks_missing_sets_invalid() {
        let baseline = report(vec![case("a", 200), case("missing", 300)]);
        let mut invalid = case("a", 100);
        invalid.validity.valid = false;
        invalid.validity.reasons.push("noise".into());
        let comparison = compare_reports(&baseline, &report(vec![invalid])).unwrap();
        assert!(!comparison.valid);
        assert!(comparison.cases.is_empty());
        assert_eq!(comparison.invalid_cases.len(), 1);
        assert_eq!(comparison.missing_candidate.len(), 1);
        assert!(comparison.geometric_mean.is_none());
    }

    #[test]
    fn comparison_rejects_classification_drift() {
        let baseline = report(vec![case("a", 200)]);
        let mut changed = case("a", 100);
        changed.category = BenchmarkCategory::Call;
        assert!(matches!(
            compare_reports(&baseline, &report(vec![changed])),
            Err(CompareError::ClassificationMismatch(_))
        ));
    }

    #[test]
    fn comparison_rejects_iteration_count_drift() {
        let baseline = report(vec![case("a", 200)]);
        let mut changed = case("a", 100);
        changed.iterations_per_sample = 10;
        assert!(matches!(
            compare_reports(&baseline, &report(vec![changed])),
            Err(CompareError::IterationCountMismatch(_))
        ));
    }
}
