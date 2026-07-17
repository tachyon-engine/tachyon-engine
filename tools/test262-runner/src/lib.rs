#![allow(clippy::disallowed_methods, clippy::disallowed_types)]
//! Test262 metadata, harness, execution, and result infrastructure.
//!
//! Host filesystem traversal belongs to [`suite`]. Engine adapters consume only owned or borrowed
//! in-memory inputs, so Tachyon's engine crates never depend on this tool or perform ambient I/O.

mod compare;
mod config;
mod edition;
mod harness;
mod metadata;
mod runner;
pub mod suite;
mod tachyon;

pub use compare::{CompareError, ReportDiff, ResultChange, TestKey, TestState, compare_reports};
pub use config::{
    ConfigError, FeatureDisposition, FeaturePolicy, PinnedProposal, ReleaseTarget, Test262Config,
};
pub use edition::{
    Applicability, ClassificationError, FeatureCatalog, FeatureStatus, SpecEdition,
    TestClassification,
};
pub use harness::{ComposedTest, Harness, HarnessError, SourceUnit};
pub use metadata::{
    FrontmatterError, NegativeExpectation, Phase, TestFlag, TestMetadata, TestVariant,
    VariantError, VariantKind,
};
pub use runner::{
    EngineAdapter, EngineOutcome, EngineResponse, ExecutionRequest, ResultKind, StubAdapter,
    TestResult, run_test,
};
pub use suite::{RunOptions, RunReport, RunSummary, SuiteError, run_checkout};
pub use tachyon::TachyonAdapter;
