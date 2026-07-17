#![allow(clippy::disallowed_methods, clippy::disallowed_types)]
#![warn(missing_docs)]
//! Reproducible multi-engine JavaScript benchmark measurement infrastructure.

mod adapter;
mod compare;
mod config;
mod external;
mod runner;
mod stats;
mod tachyon;

pub use adapter::{
    AdapterError, BenchmarkAdapter, BenchmarkRequest, EngineIdentity, EngineKind, SampleMetrics,
};
pub use compare::{BenchmarkComparison, CaseKey, CaseRatio, CompareError, compare_reports};
pub use config::{
    BenchmarkCategory, BenchmarkConfig, BuildConfig, ConfigError, CorpusError, CorpusScript,
    ExternalBuildStep, ExternalEngineProfile, ScriptConfig, SuiteKind, TachyonBenchmarkConfig,
    load_corpus,
};
pub use external::{ExternalProcessAdapter, ExternalProcessConfig};
pub use runner::{
    BENCHMARK_REPORT_SCHEMA_VERSION, BenchmarkCaseResult, BenchmarkReport, EnvironmentCheck,
    HostMetadata, MeasurementMode, RunError, Validity, run_case,
};
pub use stats::{SampleSummary, StatisticsError, summarize_samples};
pub use tachyon::{TachyonInProcessAdapter, TachyonInProcessConfig};
