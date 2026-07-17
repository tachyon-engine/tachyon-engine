use std::{collections::BTreeSet, fs, path::Path, sync::Arc};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Complete measurement, build, and approved corpus configuration.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkConfig {
    /// TOML/report contract version.
    pub schema_version: u32,
    /// Minimum retained samples required for a valid case.
    pub minimum_samples: usize,
    /// Raw samples collected after warmup.
    pub collected_samples: usize,
    /// Untimed adapter iterations before collection.
    pub warmup_iterations: usize,
    /// Fixed absolute-deviation cutoff in MAD units.
    pub outlier_mad_multiplier: f64,
    /// Noise gate applied to retained MAD/median.
    pub maximum_relative_mad: f64,
    /// Whether a single pinned CPU is mandatory for valid samples.
    pub require_cpu_affinity: bool,
    /// Required Linux scaling governor; empty disables the requirement.
    pub required_performance_governor: Box<str>,
    /// Number of host-noise calibration samples.
    pub background_precheck_samples: usize,
    /// Deterministic arithmetic work units per calibration sample.
    pub background_work_units: u64,
    /// Maximum calibration MAD/median ratio.
    pub maximum_background_relative_mad: f64,
    /// Hard deadline for one external cold-start sample.
    pub external_process_timeout_millis: u64,
    /// Per-stream external diagnostic capture cap.
    pub maximum_process_output_bytes: usize,
    /// Reproducible Tachyon/Rust build settings.
    pub build: BuildConfig,
    /// Approved content-addressed corpus entries.
    pub scripts: Vec<ScriptConfig>,
}

/// Reproducible compiler/profile identity recorded beside every result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildConfig {
    /// Cargo/build profile.
    pub profile: Box<str>,
    /// Panic strategy.
    pub panic: Box<str>,
    /// Link-time optimization mode.
    pub lto: Box<str>,
    /// Code generation unit count.
    pub codegen_units: u32,
    /// CPU feature targeting policy.
    pub target_cpu: Box<str>,
    /// Enabled Cargo feature set.
    pub features: Box<str>,
}

/// One content-addressed approved JavaScript benchmark.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptConfig {
    /// Stable corpus-relative benchmark ID.
    pub id: Box<str>,
    /// Workspace-relative checked-in path.
    pub path: Box<str>,
    /// Microbenchmark subsystem category.
    pub category: BenchmarkCategory,
    /// Aggregate suite family.
    pub suite: SuiteKind,
    /// Canonical upstream repository.
    pub source_repository: Box<str>,
    /// Full upstream source revision.
    pub source_commit: Box<str>,
    /// Path at the upstream revision.
    pub source_path: Box<str>,
    /// SHA-256 of exact checked-in bytes.
    pub sha256: Box<str>,
    /// SPDX-style source license expression.
    pub license: Box<str>,
}

/// Stable microbenchmark ownership category.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BenchmarkCategory {
    /// Frontend parse throughput.
    Parse,
    /// Bytecode compilation throughput.
    Compile,
    /// Interpreter dispatch loops.
    Dispatch,
    /// Numeric operations.
    Arithmetic,
    /// Function call/return.
    Call,
    /// Closure creation or invocation.
    Closure,
    /// Own-property access and mutation.
    Property,
    /// Prototype traversal and invalidation.
    Prototype,
    /// Array/element operations.
    Array,
    /// String operations.
    String,
    /// Regular expression operations.
    Regexp,
    /// JSON parse/stringify.
    Json,
    /// Promise and microtask operations.
    Promise,
    /// Synchronous host calls.
    HostSync,
    /// Asynchronous host completion.
    HostAsync,
    /// Object/storage allocation.
    Allocation,
    /// Garbage collection.
    Gc,
}

/// Corpus family used for separate micro/suite aggregate reporting.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SuiteKind {
    /// Focused subsystem microbenchmarks.
    Micro,
    /// V8-derived scripts redistributed by Boa.
    BoaV8,
    /// Approved js-engine-zoo comparable suite.
    JsEngineZoo,
}

/// Verified source loaded from the checked-in corpus.
#[derive(Clone, Debug)]
pub struct CorpusScript {
    /// Provenance and category metadata.
    pub config: ScriptConfig,
    /// Verified shared source bytes.
    pub source: Arc<str>,
}

/// TOML syntax or shape is invalid.
#[derive(Debug)]
pub struct ConfigError(toml::de::Error);

/// Corpus path, UTF-8, allocation, or content hash is invalid.
#[derive(Debug)]
pub struct CorpusError {
    /// Config or corpus path.
    pub path: Box<str>,
    /// Structured validation diagnostic.
    pub message: Box<str>,
}

impl BenchmarkConfig {
    /// Parses strict TOML and rejects unknown fields.
    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        toml::from_str(source).map_err(ConfigError)
    }

    /// Rejects ambiguous samples, invalid robust-statistic thresholds, duplicate IDs, and weak provenance.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != 1 {
            return Err("unsupported benchmark config schema version");
        }
        if self.minimum_samples < 10 || self.collected_samples < self.minimum_samples {
            return Err("benchmark must collect at least ten retained-capable samples");
        }
        if !self.outlier_mad_multiplier.is_finite() || self.outlier_mad_multiplier <= 0.0 {
            return Err("outlier MAD multiplier must be finite and positive");
        }
        if !self.maximum_relative_mad.is_finite() || self.maximum_relative_mad <= 0.0 {
            return Err("maximum relative MAD must be finite and positive");
        }
        if self.background_precheck_samples < 10 || self.background_work_units == 0 {
            return Err("background precheck requires at least ten non-empty samples");
        }
        if !self.maximum_background_relative_mad.is_finite()
            || self.maximum_background_relative_mad <= 0.0
        {
            return Err("maximum background relative MAD must be finite and positive");
        }
        if self.external_process_timeout_millis == 0 || self.maximum_process_output_bytes == 0 {
            return Err("external process timeout and output limit must be nonzero");
        }
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for script in &self.scripts {
            if !ids.insert(&script.id) || !paths.insert(&script.path) {
                return Err("benchmark script IDs and paths must be unique");
            }
            if !is_hex(&script.source_commit, 40) || !is_hex(&script.sha256, 64) {
                return Err(
                    "benchmark provenance requires full lowercase commit and SHA-256 hashes",
                );
            }
            if script.license.is_empty() || script.source_repository.is_empty() {
                return Err("benchmark provenance requires repository and license");
            }
        }
        Ok(())
    }
}

/// Reads every approved source, verifies content before publication, and shares bytes across adapters.
pub fn load_corpus(
    workspace: &Path,
    config: &BenchmarkConfig,
) -> Result<Vec<CorpusScript>, CorpusError> {
    config.validate().map_err(|message| CorpusError {
        path: "benchmark_config.toml".into(),
        message: message.into(),
    })?;
    let mut corpus = Vec::new();
    corpus
        .try_reserve_exact(config.scripts.len())
        .map_err(|_| CorpusError {
            path: "benchmark_config.toml".into(),
            message: "corpus allocation failed".into(),
        })?;
    for script in &config.scripts {
        let path = workspace.join(&*script.path);
        let source = fs::read_to_string(&path).map_err(|error| CorpusError {
            path: script.path.clone(),
            message: error.to_string().into(),
        })?;
        let actual = format!("{:x}", Sha256::digest(source.as_bytes()));
        if actual != script.sha256.as_ref() {
            return Err(CorpusError {
                path: script.path.clone(),
                message: format!("SHA-256 mismatch: expected {}, got {actual}", script.sha256)
                    .into(),
            });
        }
        corpus.push(CorpusScript {
            config: script.clone(),
            source: source.into(),
        });
    }
    Ok(corpus)
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "invalid benchmark config: {}", self.0)
    }
}

impl std::error::Error for ConfigError {}

impl core::fmt::Display for CorpusError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for CorpusError {}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{BenchmarkConfig, load_corpus};

    const CONFIG: &str = include_str!("../../../benchmark_config.toml");

    #[test]
    fn repository_config_and_all_corpus_hashes_are_valid() {
        let config = BenchmarkConfig::parse(CONFIG).unwrap();
        config.validate().unwrap();
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let corpus = load_corpus(&workspace, &config).unwrap();
        assert_eq!(corpus.len(), 3);
        assert!(corpus.iter().all(|script| !script.source.is_empty()));
    }
}
