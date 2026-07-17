use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::{
    AdapterError, BenchmarkAdapter, BenchmarkConfig, BenchmarkRequest, CorpusScript,
    EngineIdentity, SampleSummary, StatisticsError, summarize_samples,
};

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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HostMetadata {
    /// Operating system target.
    pub os: Box<str>,
    /// CPU architecture target.
    pub architecture: Box<str>,
    /// Host CPU model or explicit unavailable marker.
    pub cpu: Box<str>,
    /// Complete `rustc -Vv` output or unavailable marker.
    pub rustc: Box<str>,
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
    /// Timing boundary.
    pub mode: MeasurementMode,
    /// Engine and build identity.
    pub engine: EngineIdentity,
    /// Raw post-warmup durations.
    pub samples_ns: Vec<u64>,
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
}

/// Performs warmup, collects the configured fixed sample count, and marks noisy cases invalid.
pub fn run_case(
    adapter: &mut dyn BenchmarkAdapter,
    script: &CorpusScript,
    mode: MeasurementMode,
    config: &BenchmarkConfig,
) -> Result<BenchmarkCaseResult, RunError> {
    let request = BenchmarkRequest {
        script_id: script.config.id.clone(),
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
    for _ in 0..config.collected_samples {
        let metrics = adapter.sample(&request).map_err(RunError::Adapter)?;
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
    let mut reasons = Vec::new();
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
        mode,
        engine: adapter.identity().clone(),
        samples_ns: samples,
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
    pub fn collect() -> Self {
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
        }
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
        SampleMetrics, load_corpus,
    };

    use super::{MeasurementMode, run_case};

    const CONFIG: &str = include_str!("../../../benchmark_config.toml");

    struct FixtureAdapter {
        identity: EngineIdentity,
        samples: VecDeque<u64>,
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
        };
        let result = run_case(
            &mut adapter,
            &corpus[0],
            MeasurementMode::SteadyState,
            &config,
        )
        .unwrap();
        assert_eq!(result.samples_ns, [100; 15]);
        assert_eq!(result.summary.median_ns, 100);
        assert_eq!(result.peak_rss_bytes, Some(4096));
        assert!(result.validity.valid);
    }
}
