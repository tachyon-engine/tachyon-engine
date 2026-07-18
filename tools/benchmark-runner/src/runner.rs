use std::{hint::black_box, process::Command, time::Instant};

use serde::{Deserialize, Serialize};

use crate::{
    AdapterError, BenchmarkAdapter, BenchmarkConfig, BenchmarkRequest, CorpusScript,
    EngineIdentity, SampleSummary, ScriptEntry, StatisticsError, summarize_samples,
};

/// Current JSON contract for benchmark reports and derived comparisons.
pub const BENCHMARK_REPORT_SCHEMA_VERSION: u32 = 3;

/// Mutually exclusive benchmark timing boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeasurementMode {
    /// New process/isolate plus parse, compile, and one execution.
    ColdStart,
    /// Existing process with parse, compile, and one execution timed.
    ParseCompileExecute,
    /// Prebuilt bytecode/function execution without parse/compile.
    PrecompiledExecute,
    /// Warm isolate repeated execution throughput.
    SteadyState,
}

/// Host/compiler/build evidence required to interpret or reproduce a report.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HostMetadata {
    /// Operating system target.
    pub os: Box<str>,
    /// CPU architecture target.
    pub architecture: Box<str>,
    /// Host CPU model or explicit unavailable marker.
    pub cpu: Box<str>,
    /// Complete `rustc -Vv` output or unavailable marker.
    pub rustc: Box<str>,
    /// CPU pinning probe.
    pub cpu_affinity: EnvironmentCheck,
    /// CPU frequency governor probe.
    pub performance_governor: EnvironmentCheck,
    /// Robust background calibration summary.
    pub background_noise: Option<SampleSummary>,
    /// Combined host gate used by every case.
    pub validity: Validity,
}

/// One host precondition probe and its exact diagnostic.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnvironmentCheck {
    /// Whether the configured condition is met.
    pub satisfied: bool,
    /// Observed affinity/governor state or unavailable reason.
    pub detail: Box<str>,
}

/// Explicit reasons a measurement is valid or must not enter performance gates.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Validity {
    /// Whether this case may enter performance gates.
    pub valid: bool,
    /// Every failed noise/environment condition.
    pub reasons: Vec<Box<str>>,
}

/// One script/mode/engine measurement with raw samples and robust statistics.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkCaseResult {
    /// Approved benchmark ID.
    pub script_id: Box<str>,
    /// Verified source hash.
    pub script_sha256: Box<str>,
    /// Exact setup/workload entry contract.
    pub entry: ScriptEntry,
    /// Subsystem category used for aggregate ratios.
    pub category: crate::BenchmarkCategory,
    /// Corpus suite used for aggregate ratios.
    pub suite: crate::SuiteKind,
    /// Timing boundary.
    pub mode: MeasurementMode,
    /// Engine and build identity.
    pub engine: EngineIdentity,
    /// Raw post-warmup durations.
    pub samples_ns: Vec<u64>,
    /// Exact JavaScript executions represented by every raw sample.
    pub iterations_per_sample: u64,
    /// Maximum adapter-reported resident bytes.
    pub peak_rss_bytes: Option<u64>,
    /// Robust retained-sample summary.
    pub summary: SampleSummary,
    /// Noise and environment gate decision.
    pub validity: Validity,
}

/// Versioned collection of independently reproducible benchmark cases.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkReport {
    /// Machine-readable report schema.
    pub schema_version: u32,
    /// Host identity shared by cases.
    pub host: HostMetadata,
    /// Fixed build configuration.
    pub build: crate::BuildConfig,
    /// Independently reproducible script/mode/engine cases.
    pub cases: Vec<BenchmarkCaseResult>,
}

/// A complete case failed before a statistically valid report could be produced.
#[derive(Debug)]
pub enum RunError {
    /// Adapter setup or execution failed.
    Adapter(AdapterError),
    /// Samples could not produce a robust summary.
    Statistics(StatisticsError),
    /// Fixed-capacity sample/result storage could not be allocated.
    AllocationFailed,
    /// An adapter returned zero or changed its work count across samples.
    IterationCount {
        /// First nonzero count, or zero before the first valid sample.
        expected: u64,
        /// Invalid count returned by the adapter.
        actual: u64,
    },
}

/// Performs warmup, collects the configured fixed sample count, and marks noisy cases invalid.
pub fn run_case(
    adapter: &mut dyn BenchmarkAdapter,
    script: &CorpusScript,
    mode: MeasurementMode,
    config: &BenchmarkConfig,
    host: &HostMetadata,
) -> Result<BenchmarkCaseResult, RunError> {
    let request = BenchmarkRequest {
        script_id: script.config.id.clone(),
        entry: script.config.entry,
        source: script.source.clone(),
        mode,
    };
    adapter.prepare(&request).map_err(RunError::Adapter)?;
    for _ in 0..config.warmup_iterations {
        adapter.sample(&request).map_err(RunError::Adapter)?;
    }
    let mut samples = Vec::new();
    samples
        .try_reserve_exact(config.collected_samples)
        .map_err(|_| RunError::AllocationFailed)?;
    let mut peak_rss_bytes: Option<u64> = None;
    let mut iterations_per_sample = None;
    for _ in 0..config.collected_samples {
        let metrics = adapter.sample(&request).map_err(RunError::Adapter)?;
        match iterations_per_sample {
            None if metrics.iterations != 0 => iterations_per_sample = Some(metrics.iterations),
            Some(expected) if metrics.iterations == expected => {}
            expected => {
                return Err(RunError::IterationCount {
                    expected: expected.unwrap_or(0),
                    actual: metrics.iterations,
                });
            }
        }
        samples.push(metrics.elapsed_ns);
        peak_rss_bytes = match (peak_rss_bytes, metrics.peak_rss_bytes) {
            (Some(current), Some(sample)) => Some(current.max(sample)),
            (None, Some(sample)) => Some(sample),
            (current, None) => current,
        };
    }
    let summary = summarize_samples(
        &samples,
        config.minimum_samples,
        config.outlier_mad_multiplier,
    )
    .map_err(RunError::Statistics)?;
    let mut reasons = host.validity.reasons.clone();
    if summary.relative_mad > config.maximum_relative_mad {
        reasons.push(
            format!(
                "relative MAD {:.6} exceeds {:.6}",
                summary.relative_mad, config.maximum_relative_mad
            )
            .into(),
        );
    }
    Ok(BenchmarkCaseResult {
        script_id: script.config.id.clone(),
        script_sha256: script.config.sha256.clone(),
        entry: script.config.entry,
        category: script.config.category,
        suite: script.config.suite,
        mode,
        engine: adapter.identity().clone(),
        samples_ns: samples,
        iterations_per_sample: iterations_per_sample.unwrap_or(0),
        peak_rss_bytes,
        summary,
        validity: Validity {
            valid: reasons.is_empty(),
            reasons,
        },
    })
}

impl HostMetadata {
    /// Captures explicit host facts; unavailable CPU/compiler probes remain visible strings, never defaults.
    pub fn collect(config: &BenchmarkConfig) -> Self {
        let cpu_affinity = probe_cpu_affinity(config.require_cpu_affinity);
        let performance_governor =
            probe_performance_governor(&config.required_performance_governor);
        let background_noise = background_precheck(config).ok();
        let validity = host_validity(
            &cpu_affinity,
            &performance_governor,
            background_noise.as_ref(),
            config.maximum_background_relative_mad,
        );
        Self {
            os: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            cpu: cpu_name()
                .unwrap_or_else(|| "unavailable".to_owned())
                .into(),
            rustc: command_text({
                let mut command = Command::new("rustc");
                command.arg("-Vv");
                command
            })
            .unwrap_or_else(|| "unavailable".to_owned())
            .into(),
            cpu_affinity,
            performance_governor,
            background_noise,
            validity,
        }
    }

    /// Compares reproducibility identity without treating per-run probes as machine identity.
    #[must_use]
    pub fn same_static_identity(&self, other: &Self) -> bool {
        self.os == other.os
            && self.architecture == other.architecture
            && self.cpu == other.cpu
            && self.rustc == other.rustc
    }
}

/// Combines host preconditions while preserving every failed condition in report order.
fn host_validity(
    cpu_affinity: &EnvironmentCheck,
    performance_governor: &EnvironmentCheck,
    background_noise: Option<&SampleSummary>,
    maximum_background_relative_mad: f64,
) -> Validity {
    let mut reasons = Vec::new();
    if !cpu_affinity.satisfied {
        reasons.push(format!("CPU affinity: {}", cpu_affinity.detail).into());
    }
    if !performance_governor.satisfied {
        reasons.push(format!("performance governor: {}", performance_governor.detail).into());
    }
    match background_noise {
        Some(summary) if summary.relative_mad <= maximum_background_relative_mad => {}
        Some(summary) => reasons.push(
            format!(
                "background relative MAD {:.6} exceeds {:.6}",
                summary.relative_mad, maximum_background_relative_mad
            )
            .into(),
        ),
        None => reasons.push("background noise precheck failed".into()),
    }
    Validity {
        valid: reasons.is_empty(),
        reasons,
    }
}

/// Measures a deterministic host-only loop so heavily perturbed machines cannot enter parity gates.
fn background_precheck(config: &BenchmarkConfig) -> Result<SampleSummary, StatisticsError> {
    let mut samples = Vec::new();
    samples
        .try_reserve_exact(config.background_precheck_samples)
        .map_err(|_| StatisticsError::AllocationFailed)?;
    for sample_index in 0..config.background_precheck_samples {
        let start = Instant::now();
        let mut state = sample_index as u64 ^ 0x9e37_79b9_7f4a_7c15;
        for _ in 0..config.background_work_units {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            black_box(state);
        }
        samples.push(start.elapsed().as_nanos().try_into().unwrap_or(u64::MAX));
    }
    summarize_samples(&samples, 10, config.outlier_mad_multiplier)
}

#[cfg(target_os = "linux")]
fn probe_cpu_affinity(required: bool) -> EnvironmentCheck {
    let mut command = Command::new("taskset");
    command.args(["-pc", &std::process::id().to_string()]);
    let detail = command_text(command).unwrap_or_else(|| "taskset unavailable".to_owned());
    let cpu_list = detail.rsplit_once(':').map(|(_, value)| value.trim());
    let pinned = cpu_list.is_some_and(|value| !value.contains(',') && !value.contains('-'));
    EnvironmentCheck {
        satisfied: !required || pinned,
        detail: detail.into(),
    }
}

#[cfg(not(target_os = "linux"))]
fn probe_cpu_affinity(required: bool) -> EnvironmentCheck {
    EnvironmentCheck {
        satisfied: !required,
        detail: "single-CPU affinity probe unavailable on this host".into(),
    }
}

#[cfg(target_os = "linux")]
fn probe_performance_governor(required: &str) -> EnvironmentCheck {
    if required.is_empty() {
        return EnvironmentCheck {
            satisfied: true,
            detail: "not required".into(),
        };
    }
    let observed = std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .map_or_else(
            |_| "unavailable".to_owned(),
            |value| value.trim().to_owned(),
        );
    EnvironmentCheck {
        satisfied: observed == required,
        detail: observed.into(),
    }
}

#[cfg(not(target_os = "linux"))]
fn probe_performance_governor(required: &str) -> EnvironmentCheck {
    EnvironmentCheck {
        satisfied: required.is_empty(),
        detail: "CPU scaling governor probe unavailable on this host".into(),
    }
}

#[cfg(target_os = "macos")]
fn cpu_name() -> Option<String> {
    let mut command = Command::new("sysctl");
    command.args(["-n", "machdep.cpu.brand_string"]);
    command_text(command)
}

#[cfg(target_os = "linux")]
fn cpu_name() -> Option<String> {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()?
        .lines()
        .find(|line| line.starts_with("model name"))
        .and_then(|line| line.split_once(':'))
        .map(|(_, name)| name.trim().to_owned())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn cpu_name() -> Option<String> {
    None
}

fn command_text(mut command: Command) -> Option<String> {
    let output = command.output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

impl core::fmt::Display for RunError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "benchmark run failed: {self:?}")
    }
}

impl std::error::Error for RunError {}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use crate::{
        BenchmarkAdapter, BenchmarkConfig, BenchmarkRequest, EngineIdentity, EngineKind,
        HostMetadata, SampleMetrics, SampleSummary, load_corpus,
    };

    use super::{EnvironmentCheck, MeasurementMode, host_validity, run_case};

    const CONFIG: &str = include_str!("../../../benchmark_config.toml");

    struct FixtureAdapter {
        identity: EngineIdentity,
        samples: VecDeque<u64>,
        iterations: VecDeque<u64>,
    }

    impl BenchmarkAdapter for FixtureAdapter {
        fn identity(&self) -> &EngineIdentity {
            &self.identity
        }

        fn prepare(&mut self, _request: &BenchmarkRequest) -> Result<(), crate::AdapterError> {
            Ok(())
        }

        fn sample(
            &mut self,
            _request: &BenchmarkRequest,
        ) -> Result<SampleMetrics, crate::AdapterError> {
            Ok(SampleMetrics {
                elapsed_ns: self.samples.pop_front().unwrap_or(100),
                iterations: self.iterations.pop_front().unwrap_or(1),
                peak_rss_bytes: Some(4096),
            })
        }
    }

    #[test]
    /// Exercises warmup exclusion, exact sample count, robust summary, RSS maximum, and identity retention.
    fn case_runner_produces_a_valid_reproducible_result() {
        let config = BenchmarkConfig::parse(CONFIG).unwrap();
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let corpus = load_corpus(&workspace, &config).unwrap();
        let mut adapter = FixtureAdapter {
            identity: EngineIdentity {
                name: "fixture".into(),
                kind: EngineKind::Fixture,
                version: "1".into(),
                commit: "fixture".into(),
                features: "none".into(),
                build_flags: "fixture".into(),
                binary_size_bytes: None,
            },
            samples: [1, 2, 3].into_iter().chain([100; 15]).collect(),
            iterations: [1; 18].into(),
        };
        let host = HostMetadata {
            os: "test".into(),
            architecture: "test".into(),
            cpu: "test".into(),
            rustc: "test".into(),
            cpu_affinity: super::EnvironmentCheck {
                satisfied: true,
                detail: "fixture".into(),
            },
            performance_governor: super::EnvironmentCheck {
                satisfied: true,
                detail: "fixture".into(),
            },
            background_noise: None,
            validity: super::Validity {
                valid: true,
                reasons: Vec::new(),
            },
        };
        let result = run_case(
            &mut adapter,
            &corpus[0],
            MeasurementMode::SteadyState,
            &config,
            &host,
        )
        .unwrap();
        assert_eq!(result.samples_ns, [100; 15]);
        assert_eq!(result.iterations_per_sample, 1);
        assert_eq!(result.summary.median_ns, 100);
        assert_eq!(result.peak_rss_bytes, Some(4096));
        assert!(result.validity.valid);
    }

    #[test]
    /// Rejects zero and changing execution counts before statistics can make workloads look comparable.
    fn case_runner_rejects_invalid_iteration_counts() {
        let mut config = BenchmarkConfig::parse(CONFIG).unwrap();
        config.warmup_iterations = 0;
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let corpus = load_corpus(&workspace, &config).unwrap();
        let identity = EngineIdentity {
            name: "fixture".into(),
            kind: EngineKind::Fixture,
            version: "1".into(),
            commit: "fixture".into(),
            features: "none".into(),
            build_flags: "fixture".into(),
            binary_size_bytes: None,
        };
        let host = HostMetadata {
            os: "test".into(),
            architecture: "test".into(),
            cpu: "test".into(),
            rustc: "test".into(),
            cpu_affinity: EnvironmentCheck {
                satisfied: true,
                detail: "fixture".into(),
            },
            performance_governor: EnvironmentCheck {
                satisfied: true,
                detail: "fixture".into(),
            },
            background_noise: None,
            validity: super::Validity {
                valid: true,
                reasons: Vec::new(),
            },
        };
        let mut changing = FixtureAdapter {
            identity: identity.clone(),
            samples: [100; 15].into(),
            iterations: [1, 2].into(),
        };
        assert!(matches!(
            run_case(
                &mut changing,
                &corpus[0],
                MeasurementMode::SteadyState,
                &config,
                &host,
            ),
            Err(super::RunError::IterationCount {
                expected: 1,
                actual: 2
            })
        ));
        let mut zero = FixtureAdapter {
            identity,
            samples: [100; 15].into(),
            iterations: [0].into(),
        };
        assert!(matches!(
            run_case(
                &mut zero,
                &corpus[0],
                MeasurementMode::SteadyState,
                &config,
                &host,
            ),
            Err(super::RunError::IterationCount {
                expected: 0,
                actual: 0
            })
        ));
    }

    #[test]
    fn host_gate_reports_unavailable_probes_and_excess_noise() {
        let unavailable = EnvironmentCheck {
            satisfied: false,
            detail: "unavailable".into(),
        };
        let noisy = SampleSummary {
            collected: 15,
            retained: 15,
            rejected_outliers: 0,
            median_ns: 100,
            mad_ns: 10,
            relative_mad: 0.10,
            confidence_low_ns: 90,
            confidence_high_ns: 110,
            confidence_method: "fixture".into(),
        };
        let validity = host_validity(&unavailable, &unavailable, Some(&noisy), 0.05);
        assert!(!validity.valid);
        assert_eq!(validity.reasons.len(), 3);
        assert!(validity.reasons[0].contains("CPU affinity"));
        assert!(validity.reasons[1].contains("performance governor"));
        assert!(validity.reasons[2].contains("background relative MAD"));
    }

    #[test]
    fn host_gate_accepts_satisfied_probes_and_quiet_background() {
        let satisfied = EnvironmentCheck {
            satisfied: true,
            detail: "fixture".into(),
        };
        let quiet = SampleSummary {
            collected: 15,
            retained: 15,
            rejected_outliers: 0,
            median_ns: 100,
            mad_ns: 1,
            relative_mad: 0.01,
            confidence_low_ns: 99,
            confidence_high_ns: 101,
            confidence_method: "fixture".into(),
        };
        assert!(host_validity(&satisfied, &satisfied, Some(&quiet), 0.05).valid);
    }
}
