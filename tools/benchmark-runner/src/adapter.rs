use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{MeasurementMode, ScriptEntry};

/// Engine family and integration boundary used for fair grouping.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineKind {
    /// Tachyon linked directly into the runner process.
    TachyonInProcess,
    /// Boa linked directly into the runner process.
    BoaInProcess,
    /// Boa release command-line executable.
    BoaCli,
    /// QuickJS release command-line executable.
    QuickJsCli,
    /// Escargot release command-line executable.
    EscargotCli,
    /// Deterministic test-only implementation.
    Fixture,
}

/// Immutable engine/build identity repeated in every standalone report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EngineIdentity {
    /// Stable report/display name.
    pub name: Box<str>,
    /// Integration and engine family.
    pub kind: EngineKind,
    /// Engine/compiler version string.
    pub version: Box<str>,
    /// Source revision, when available.
    pub commit: Box<str>,
    /// Enabled runtime or compile-time features.
    pub features: Box<str>,
    /// Exact release build flags.
    pub build_flags: Box<str>,
    /// Measured executable size for process adapters.
    pub binary_size_bytes: Option<u64>,
}

/// One adapter invocation after source/provenance validation.
#[derive(Clone, Debug)]
pub struct BenchmarkRequest {
    /// Approved corpus ID.
    pub script_id: Box<str>,
    /// Versioned execution entry contract from the approved corpus.
    pub entry: ScriptEntry,
    /// Verified immutable JavaScript source.
    pub source: Arc<str>,
    /// Exact timing boundary requested from the adapter.
    pub mode: MeasurementMode,
    /// Exact complete JavaScript workload executions required from each sample.
    pub iterations: u64,
}

/// Timing and optional process-memory evidence for one completed sample.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SampleMetrics {
    /// Complete sample duration in nanoseconds.
    pub elapsed_ns: u64,
    /// Exact JavaScript executions represented by this duration.
    pub iterations: u64,
    /// Peak resident bytes, when the adapter can measure it.
    pub peak_rss_bytes: Option<u64>,
}

/// Adapter setup, unsupported mode, engine error, timeout, or abnormal process exit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdapterError {
    /// Adapter cannot honestly implement this timing boundary.
    UnsupportedMode(MeasurementMode),
    /// Parse/compile/process preparation failed.
    Setup(Box<str>),
    /// In-process compilation or execution failed inside the timed boundary.
    Engine(Box<str>),
    /// JavaScript execution returned a normal nonzero exit status.
    Execution {
        /// Numeric exit status.
        status: i32,
        /// Captured standard output, capped by adapter policy.
        stdout: Box<str>,
        /// Captured standard error, capped by adapter policy.
        stderr: Box<str>,
    },
    /// Configured deadline expired and the child was terminated.
    Timeout {
        /// Deadline and termination diagnostic.
        message: Box<str>,
        /// Output produced before termination, capped by adapter policy.
        stdout: Box<str>,
        /// Error output produced before termination, capped by adapter policy.
        stderr: Box<str>,
    },
    /// External process terminated abnormally.
    Crash {
        /// Exit status or signal diagnostic.
        message: Box<str>,
        /// Captured standard output, capped by adapter policy.
        stdout: Box<str>,
        /// Captured standard error.
        stderr: Box<str>,
    },
}

/// Stateful serial adapter; benchmark cases never share one mutable isolate across threads.
pub trait BenchmarkAdapter {
    /// Returns immutable engine/build identity.
    fn identity(&self) -> &EngineIdentity;

    /// Performs mode-specific parse/compile/preparation outside timed samples when the mode permits it.
    fn prepare(&mut self, request: &BenchmarkRequest) -> Result<(), AdapterError>;

    /// Executes exactly one timed sample under the prepared mode contract.
    fn sample(&mut self, request: &BenchmarkRequest) -> Result<SampleMetrics, AdapterError>;
}

pub(crate) const MAIN_INVOCATION_SOURCE: &str = "main();";

/// Composes one process/parse workload while retaining the approved source as separate provenance.
pub(crate) fn compose_execution_source(
    source: &Arc<str>,
    entry: ScriptEntry,
) -> Result<Arc<str>, AdapterError> {
    if entry == ScriptEntry::Script {
        return Ok(Arc::clone(source));
    }
    const PREFIX: &str = "\n;";
    const SUFFIX: &str = "\n";
    let capacity = source
        .len()
        .checked_add(PREFIX.len())
        .and_then(|length| length.checked_add(MAIN_INVOCATION_SOURCE.len()))
        .and_then(|length| length.checked_add(SUFFIX.len()))
        .ok_or_else(|| AdapterError::Setup("benchmark workload source is too large".into()))?;
    let mut composed = String::new();
    composed
        .try_reserve_exact(capacity)
        .map_err(|_| AdapterError::Setup("benchmark workload source allocation failed".into()))?;
    composed.push_str(source);
    composed.push_str(PREFIX);
    composed.push_str(MAIN_INVOCATION_SOURCE);
    composed.push_str(SUFFIX);
    Ok(composed.into())
}

impl core::fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "benchmark adapter error: {self:?}")
    }
}

impl std::error::Error for AdapterError {}
