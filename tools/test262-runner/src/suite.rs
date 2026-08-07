//! Filesystem-backed Test262 checkout scanning and deterministic execution.

use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    Applicability, EngineAdapter, FeatureCatalog, Harness, Phase, ResultKind, SourceUnit,
    SpecEdition, Test262Config, TestClassification, TestMetadata, TestResult, run_test,
};

/// Stable traversal, selection, scheduling, and randomization policy for one suite run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunOptions {
    /// Checkout-relative directory or file; `None` selects the complete `test` tree.
    pub selector: Option<PathBuf>,
    /// Optional checkout-relative path substring filter.
    pub filter: Option<Box<str>>,
    /// Whether independent tests may execute through Rayon.
    pub parallel: bool,
    /// Deterministic execution-order seed.
    pub seed: Option<u64>,
    /// Whether Git HEAD and tracked cleanliness must match the manifest.
    pub verify_commit: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            selector: None,
            filter: None,
            parallel: false,
            seed: None,
            verify_commit: true,
        }
    }
}

/// Versioned machine-readable output and all independently counted strictness variants.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunReport {
    /// Machine-readable report schema version.
    pub schema_version: u32,
    /// Verified upstream revision.
    pub test262_commit: Box<str>,
    /// SHA-256 of the canonical release-target and feature-policy configuration.
    pub policy_sha256: Box<str>,
    /// Optional deterministic randomization seed.
    pub seed: Option<u64>,
    /// Aggregate counts retaining all failure categories.
    pub summary: RunSummary,
    /// Stable path/variant-ordered individual results.
    pub results: Vec<TestResult>,
}

/// Aggregate categories that never remove unsupported or infrastructure failures from total.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunSummary {
    /// All independently counted variants, including unsupported and infrastructure failures.
    pub total: u64,
    /// Variants whose actual outcome matched their expectation.
    pub passed: u64,
    /// Variants in the release-target denominator.
    pub applicable_total: u64,
    /// Passing variants in the release-target denominator.
    pub applicable_passed: u64,
    /// Global category counts.
    pub by_result: BTreeMap<ResultKind, u64>,
    /// Counts grouped by positive/parse/resolution/runtime expectation.
    pub by_phase: BTreeMap<Box<str>, BTreeMap<ResultKind, u64>>,
    /// Counts grouped by minimum ECMAScript edition.
    pub by_edition: BTreeMap<SpecEdition, BTreeMap<ResultKind, u64>>,
    /// Counts grouped by denominator applicability.
    pub by_applicability: BTreeMap<Applicability, BTreeMap<ResultKind, u64>>,
    /// Counts grouped by every upstream feature tag.
    pub by_feature: BTreeMap<Box<str>, BTreeMap<ResultKind, u64>>,
    /// Counts grouped by the first two checkout path components.
    pub by_path: BTreeMap<Box<str>, BTreeMap<ResultKind, u64>>,
}

/// Optional execution diagnostics emitted around each source file without changing report order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunProgress<'a> {
    /// The runner is about to execute all strictness variants for one file.
    Started {
        path: &'a str,
        started: usize,
        total: usize,
    },
    /// Every variant for one file completed.
    Completed {
        path: &'a str,
        completed: usize,
        total: usize,
        variants: usize,
        elapsed: Duration,
    },
}

/// Thread-safe sink used only when a caller explicitly requests live progress diagnostics.
pub trait ProgressObserver: Sync {
    /// Receives one non-persisted execution boundary event.
    fn observe(&self, progress: RunProgress<'_>);
}

/// A checkout path, metadata block, harness include, or variant could not be loaded safely.
#[derive(Debug)]
pub struct SuiteError {
    /// Checkout, test, harness, selector, or config path involved.
    pub path: PathBuf,
    /// Stable diagnostic without a panic.
    pub message: Box<str>,
}

#[derive(Debug)]
struct LoadedTest {
    relative_path: Box<str>,
    source: Arc<str>,
    metadata: TestMetadata,
    classification: TestClassification,
    modules: Vec<SourceUnit>,
}

/// Loads the checkout, executes all selected variants, and restores path/variant order after parallel work.
pub fn run_checkout(
    checkout: &Path,
    config: &Test262Config,
    adapter: &dyn EngineAdapter,
    options: &RunOptions,
) -> Result<RunReport, SuiteError> {
    run_checkout_with_progress(checkout, config, adapter, options, None)
}

/// Executes a checkout while reporting file boundaries so a long-running test remains identifiable.
pub fn run_checkout_with_progress(
    checkout: &Path,
    config: &Test262Config,
    adapter: &dyn EngineAdapter,
    options: &RunOptions,
    observer: Option<&dyn ProgressObserver>,
) -> Result<RunReport, SuiteError> {
    config.validate().map_err(|message| SuiteError {
        path: PathBuf::from("test262_config.toml"),
        message: message.into(),
    })?;
    if options.verify_commit {
        verify_checkout_commit(checkout, &config.commit)?;
    }
    let harness = load_harness(checkout)?;
    let feature_catalog = FeatureCatalog::parse(&read_utf8(&checkout.join("features.txt"))?);
    let mut tests = load_tests(checkout, config, &feature_catalog, options)?;
    if let Some(seed) = options.seed {
        tests.sort_by(|left, right| {
            seeded_key(seed, &left.relative_path)
                .cmp(&seeded_key(seed, &right.relative_path))
                .then(left.relative_path.cmp(&right.relative_path))
        });
    }
    let total = tests.len();
    let started = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    let execute = |test: &LoadedTest| {
        let ordinal = started.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(observer) = observer {
            observer.observe(RunProgress::Started {
                path: &test.relative_path,
                started: ordinal,
                total,
            });
        }
        let start = Instant::now();
        let result = execute_loaded(test, &harness, adapter);
        if let (Some(observer), Ok(variants)) = (observer, &result) {
            observer.observe(RunProgress::Completed {
                path: &test.relative_path,
                completed: completed.fetch_add(1, Ordering::Relaxed) + 1,
                total,
                variants: variants.len(),
                elapsed: start.elapsed(),
            });
        }
        result
    };
    let nested = if options.parallel {
        tests
            .par_iter()
            .with_max_len(1)
            .map(execute)
            .collect::<Result<Vec<_>, _>>()?
    } else {
        tests.iter().map(execute).collect::<Result<Vec<_>, _>>()?
    };
    let mut results = nested.into_iter().flatten().collect::<Vec<_>>();
    results.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.variant.cmp(&right.variant))
    });
    let summary = summarize(&results);
    Ok(RunReport {
        schema_version: 2,
        test262_commit: config.commit.clone(),
        policy_sha256: config.policy_fingerprint().map_err(|error| SuiteError {
            path: PathBuf::from("test262_config.toml"),
            message: error.to_string().into(),
        })?,
        seed: options.seed,
        summary,
        results,
    })
}

/// Reads standard and nested harness sources under stable slash-separated names.
fn load_harness(checkout: &Path) -> Result<Harness, SuiteError> {
    let root = checkout.join("harness");
    let mut paths = Vec::new();
    collect_js_files(&root, &mut paths)?;
    paths.sort();
    let mut harness = Harness::new();
    for path in paths {
        let name = relative_utf8(&root, &path)?;
        let source = read_utf8(&path)?;
        harness.insert(name, source);
    }
    Ok(harness)
}

/// Selects either the complete test tree, one directory, or one file, then parses every metadata block.
fn load_tests(
    checkout: &Path,
    config: &Test262Config,
    feature_catalog: &FeatureCatalog,
    options: &RunOptions,
) -> Result<Vec<LoadedTest>, SuiteError> {
    let test_root = checkout.join("test");
    let selected = match options.selector.as_ref() {
        None => test_root,
        Some(path) if is_checkout_relative(path) => checkout.join(path),
        Some(path) => {
            return Err(SuiteError {
                path: path.clone(),
                message: "selector must be a relative path within the test262 checkout".into(),
            });
        }
    };
    let mut paths = Vec::new();
    if selected.is_file() {
        paths.push(selected);
    } else {
        collect_js_files(&selected, &mut paths)?;
    }
    paths.sort();
    let mut tests = Vec::with_capacity(paths.len());
    let mut fixture_cache: BTreeMap<PathBuf, Vec<SourceUnit>> = BTreeMap::new();
    for path in paths {
        if path
            .file_stem()
            .is_some_and(|stem| stem.as_encoded_bytes().ends_with(b"FIXTURE"))
        {
            continue;
        }
        let relative_path = relative_utf8(checkout, &path)?;
        if options
            .filter
            .as_ref()
            .is_some_and(|filter| !relative_path.contains(&**filter))
        {
            continue;
        }
        let source = read_utf8(&path)?;
        let metadata = TestMetadata::parse(&source).map_err(|error| SuiteError {
            path: path.clone(),
            message: error.to_string().into(),
        })?;
        let classification = feature_catalog
            .classify(&metadata, &relative_path, config)
            .map_err(|error| SuiteError {
                path: path.clone(),
                message: error.to_string().into(),
            })?;
        let directory = path.parent().ok_or_else(|| SuiteError {
            path: path.clone(),
            message: "test path has no parent directory".into(),
        })?;
        if !fixture_cache.contains_key(directory) {
            fixture_cache.insert(
                directory.to_owned(),
                load_same_directory_fixtures(checkout, &path)?,
            );
        }
        tests.push(LoadedTest {
            relative_path: relative_path.into(),
            source: source.into(),
            metadata,
            classification,
            modules: fixture_cache
                .get(directory)
                .expect("fixture directory was cached")
                .clone(),
        });
    }
    Ok(tests)
}

/// Expands and executes every strictness variant while treating composition errors as scan failures.
fn execute_loaded(
    loaded: &LoadedTest,
    harness: &Harness,
    adapter: &dyn EngineAdapter,
) -> Result<Vec<TestResult>, SuiteError> {
    let variants = loaded.metadata.variants().map_err(|error| SuiteError {
        path: PathBuf::from(&*loaded.relative_path),
        message: format!("invalid Test262 flags: {error:?}").into(),
    })?;
    let mut results = Vec::with_capacity(variants.len());
    for variant in variants {
        let composed = harness
            .compose(
                &loaded.relative_path,
                Arc::clone(&loaded.source),
                &loaded.metadata,
                variant,
            )
            .map_err(|error| SuiteError {
                path: PathBuf::from(&*loaded.relative_path),
                message: error.to_string().into(),
            })?;
        let mut composed = composed;
        composed.set_modules(loaded.modules.clone());
        results.push(run_test(
            adapter,
            &loaded.relative_path,
            &loaded.metadata,
            &composed,
            &loaded.classification,
        ));
    }
    Ok(results)
}

/// Recursively collects JavaScript files without following directory symlinks.
fn collect_js_files(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), SuiteError> {
    let entries = fs::read_dir(root).map_err(|error| io_error(root, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| io_error(root, error))?;
        let file_type = entry
            .file_type()
            .map_err(|error| io_error(&entry.path(), error))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_js_files(&entry.path(), output)?;
        } else if file_type.is_file() && entry.path().extension() == Some(OsStr::new("js")) {
            output.push(entry.path());
        }
    }
    Ok(())
}

fn relative_utf8(root: &Path, path: &Path) -> Result<String, SuiteError> {
    let relative = path.strip_prefix(root).map_err(|error| SuiteError {
        path: path.to_owned(),
        message: error.to_string().into(),
    })?;
    relative
        .to_str()
        .map(|value| value.replace(std::path::MAIN_SEPARATOR, "/"))
        .ok_or_else(|| SuiteError {
            path: path.to_owned(),
            message: "test262 paths must be valid UTF-8".into(),
        })
}

fn read_utf8(path: &Path) -> Result<String, SuiteError> {
    fs::read_to_string(path).map_err(|error| io_error(path, error))
}

/// Loads adjacent Test262 fixture modules into owned memory for the engine's deterministic loader.
fn load_same_directory_fixtures(
    checkout: &Path,
    test_path: &Path,
) -> Result<Vec<SourceUnit>, SuiteError> {
    let directory = test_path.parent().ok_or_else(|| SuiteError {
        path: test_path.to_owned(),
        message: "test path has no parent directory".into(),
    })?;
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| io_error(directory, error))? {
        let entry = entry.map_err(|error| io_error(directory, error))?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| io_error(&path, error))?;
        if !file_type.is_file()
            || path.extension() != Some(OsStr::new("js"))
            || !path
                .file_stem()
                .is_some_and(|stem| stem.as_encoded_bytes().ends_with(b"FIXTURE"))
        {
            continue;
        }
        paths.push(path);
    }
    paths.sort();
    let mut modules = Vec::with_capacity(paths.len());
    for path in paths {
        modules.push(SourceUnit {
            name: relative_utf8(checkout, &path)?.into(),
            source: read_utf8(&path)?.into(),
        });
    }
    Ok(modules)
}

/// Rejects checkout drift before a report can claim results for the configured revision.
fn verify_checkout_commit(checkout: &Path, expected: &str) -> Result<(), SuiteError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|error| io_error(checkout, error))?;
    if !output.status.success() {
        return Err(SuiteError {
            path: checkout.to_owned(),
            message: String::from_utf8_lossy(&output.stderr)
                .trim()
                .to_owned()
                .into_boxed_str(),
        });
    }
    let actual = String::from_utf8_lossy(&output.stdout);
    if actual.trim() != expected {
        return Err(SuiteError {
            path: checkout.to_owned(),
            message: format!(
                "test262 checkout revision mismatch: expected {expected}, got {}",
                actual.trim()
            )
            .into(),
        });
    }
    let clean = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["diff-index", "--quiet", "HEAD", "--"])
        .status()
        .map_err(|error| io_error(checkout, error))?;
    if !clean.success() {
        return Err(SuiteError {
            path: checkout.to_owned(),
            message: "test262 checkout has tracked modifications".into(),
        });
    }
    Ok(())
}

fn is_checkout_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn io_error(path: &Path, error: std::io::Error) -> SuiteError {
    SuiteError {
        path: path.to_owned(),
        message: error.to_string().into(),
    }
}

/// Produces a deterministic pseudo-random key without relying on ambient entropy or platform hashing.
fn seeded_key(seed: u64, path: &str) -> u64 {
    let mut hash = seed ^ 0xcbf2_9ce4_8422_2325;
    for byte in path.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Aggregates exact result counts by expected phase, feature, and first two path components.
fn summarize(results: &[TestResult]) -> RunSummary {
    let mut summary = RunSummary {
        total: results.len() as u64,
        passed: results
            .iter()
            .filter(|result| result.result == ResultKind::Pass)
            .count() as u64,
        applicable_total: results
            .iter()
            .filter(|result| result.applicability == Applicability::Applicable)
            .count() as u64,
        applicable_passed: results
            .iter()
            .filter(|result| {
                result.applicability == Applicability::Applicable
                    && result.result == ResultKind::Pass
            })
            .count() as u64,
        ..RunSummary::default()
    };
    for result in results {
        increment(&mut summary.by_result, result.result);
        increment_nested(
            &mut summary.by_phase,
            phase_name(result.expected_phase).into(),
            result.result,
        );
        increment_nested(&mut summary.by_edition, result.edition, result.result);
        increment_nested(
            &mut summary.by_applicability,
            result.applicability,
            result.result,
        );
        for feature in &result.features {
            increment_nested(&mut summary.by_feature, feature.clone(), result.result);
        }
        increment_nested(
            &mut summary.by_path,
            path_group(&result.path).into(),
            result.result,
        );
    }
    summary
}

fn path_group(path: &str) -> &str {
    path.char_indices()
        .filter(|(_, character)| *character == '/')
        .nth(1)
        .map_or(path, |(index, _)| &path[..index])
}

fn phase_name(phase: Option<Phase>) -> &'static str {
    match phase {
        None => "positive",
        Some(Phase::Parse) => "parse",
        Some(Phase::Resolution) => "resolution",
        Some(Phase::Runtime) => "runtime",
    }
}

fn increment(map: &mut BTreeMap<ResultKind, u64>, result: ResultKind) {
    *map.entry(result).or_default() += 1;
}

fn increment_nested<K: Ord>(
    map: &mut BTreeMap<K, BTreeMap<ResultKind, u64>>,
    key: K,
    result: ResultKind,
) {
    increment(map.entry(key).or_default(), result);
}

impl core::fmt::Display for SuiteError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}: {}", self.path.display(), self.message)
    }
}

impl std::error::Error for SuiteError {}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Mutex};

    use tempfile::TempDir;

    use crate::{
        EngineAdapter, EngineOutcome, EngineResponse, ExecutionRequest, Phase, ResultKind,
        StubAdapter, Test262Config, VariantKind,
    };

    use super::{
        ProgressObserver, RunOptions, RunProgress, run_checkout, run_checkout_with_progress,
    };

    const CONFIG: &str = include_str!("../../../test262_config.toml");

    /// Creates a minimal checkout that exercises positive, negative, strict, module, async, and fixture paths.
    fn checkout() -> TempDir {
        let root = TempDir::new().unwrap();
        fs::create_dir_all(root.path().join("harness")).unwrap();
        fs::create_dir_all(root.path().join("test/language")).unwrap();
        fs::write(root.path().join("harness/assert.js"), "assert").unwrap();
        fs::write(root.path().join("harness/sta.js"), "sta").unwrap();
        fs::write(root.path().join("harness/doneprintHandle.js"), "done").unwrap();
        fs::write(
            root.path().join("features.txt"),
            "## Standard language features\nBigInt\n",
        )
        .unwrap();
        fs::write(
            root.path().join("test/language/positive.js"),
            "/*---\ndescription: positive\nfeatures: [BigInt]\n---*/\n1;",
        )
        .unwrap();
        fs::write(
            root.path().join("test/language/module.js"),
            "/*---\ndescription: module\nflags: [module, async]\n---*/\nexport {};",
        )
        .unwrap();
        fs::write(
            root.path().join("test/language/negative.js"),
            "/*---\ndescription: negative\nflags: [onlyStrict]\nnegative:\n  phase: parse\n  type: SyntaxError\n---*/\ninvalid",
        )
        .unwrap();
        fs::write(
            root.path().join("test/language/data_FIXTURE.js"),
            "not metadata",
        )
        .unwrap();
        root
    }

    #[test]
    /// Exercises recursive scan, fixture exclusion, strict expansion, module handling, and accounting.
    fn stub_scans_checkout_and_reports_every_variant_as_unsupported() {
        let root = checkout();
        let config = Test262Config::parse(CONFIG).unwrap();
        let report = run_checkout(
            root.path(),
            &config,
            &StubAdapter::unsupported(),
            &RunOptions {
                parallel: false,
                verify_commit: false,
                ..RunOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.summary.total, 4);
        assert_eq!(report.summary.by_result[&ResultKind::Unsupported], 4);
        assert_eq!(report.results[0].path.as_ref(), "test/language/module.js");
        assert_eq!(report.results[1].path.as_ref(), "test/language/negative.js");
        assert_eq!(report.results[2].path.as_ref(), "test/language/positive.js");
        assert_eq!(report.results[3].path.as_ref(), "test/language/positive.js");
    }

    #[test]
    /// Proves parallel scheduling and seeded input order cannot perturb persisted report order.
    fn parallel_seeded_run_preserves_stable_report_order() {
        let root = checkout();
        let config = Test262Config::parse(CONFIG).unwrap();
        let options = RunOptions {
            parallel: true,
            seed: Some(42),
            verify_commit: false,
            ..RunOptions::default()
        };
        let first =
            run_checkout(root.path(), &config, &StubAdapter::unsupported(), &options).unwrap();
        let second =
            run_checkout(root.path(), &config, &StubAdapter::unsupported(), &options).unwrap();
        assert_eq!(first, second);
        let json = serde_json::to_string(&first).unwrap();
        assert_eq!(
            serde_json::from_str::<super::RunReport>(&json).unwrap(),
            first
        );
    }

    #[derive(Default)]
    struct RecordingProgress(Mutex<Vec<(bool, String, usize)>>);

    impl ProgressObserver for RecordingProgress {
        fn observe(&self, progress: RunProgress<'_>) {
            let event = match progress {
                RunProgress::Started { path, started, .. } => (true, path.to_owned(), started),
                RunProgress::Completed {
                    path, completed, ..
                } => (false, path.to_owned(), completed),
            };
            self.0.lock().unwrap().push(event);
        }
    }

    #[test]
    fn progress_brackets_each_file_without_changing_variant_accounting() {
        let root = checkout();
        let config = Test262Config::parse(CONFIG).unwrap();
        let progress = RecordingProgress::default();
        let report = run_checkout_with_progress(
            root.path(),
            &config,
            &StubAdapter::unsupported(),
            &RunOptions {
                parallel: false,
                verify_commit: false,
                ..RunOptions::default()
            },
            Some(&progress),
        )
        .unwrap();
        let events = progress.0.into_inner().unwrap();
        assert_eq!(report.summary.total, 4);
        assert_eq!(events.len(), 6);
        let (pairs, remainder) = events.as_chunks::<2>();
        assert!(remainder.is_empty());
        assert!(pairs.iter().all(|[started, completed]| {
            started.0 && !completed.0 && started.1 == completed.1 && started.2 == completed.2
        }));
    }

    #[test]
    fn selector_cannot_escape_the_checkout() {
        let root = checkout();
        let config = Test262Config::parse(CONFIG).unwrap();
        let error = run_checkout(
            root.path(),
            &config,
            &StubAdapter::unsupported(),
            &RunOptions {
                selector: Some("../outside".into()),
                parallel: false,
                verify_commit: false,
                ..RunOptions::default()
            },
        )
        .unwrap_err();
        assert!(error.message.contains("within the test262 checkout"));
    }

    struct InspectingAdapter;

    impl EngineAdapter for InspectingAdapter {
        /// Validates the complete request contract before emulating the expected fixture outcome.
        fn execute(&self, request: ExecutionRequest<'_>) -> EngineResponse {
            let name = request.test.body.name.as_ref();
            let valid = match name {
                "test/language/module.js" => {
                    request.is_module
                        && request.is_async
                        && request.can_block
                        && request
                            .test
                            .preludes
                            .iter()
                            .any(|unit| unit.name.as_ref() == "doneprintHandle.js")
                }
                "test/language/negative.js" => {
                    request.test.variant.kind == VariantKind::Strict
                        && request.test.body.source.starts_with("\"use strict\";")
                }
                "test/language/positive.js" => {
                    !request.is_module
                        && !request.is_async
                        && match request.test.variant.kind {
                            VariantKind::Strict => {
                                request.test.body.source.starts_with("\"use strict\";")
                            }
                            VariantKind::Sloppy => {
                                !request.test.body.source.starts_with("\"use strict\";")
                            }
                            VariantKind::Module | VariantKind::Raw => false,
                        }
                }
                _ => false,
            };
            if !valid {
                return EngineResponse::new(EngineOutcome::HarnessFailure {
                    message: "invalid fake-engine request".into(),
                });
            }
            if name == "test/language/negative.js" {
                EngineResponse::new(EngineOutcome::Error {
                    phase: Phase::Parse,
                    error_type: "SyntaxError".into(),
                    message: "expected fixture error".into(),
                })
            } else {
                EngineResponse::new(EngineOutcome::Completed)
            }
        }
    }

    #[test]
    /// Proves one fake engine receives correct positive/negative/strict/async/module request semantics.
    fn fake_engine_fixture_passes_every_execution_mode() {
        let root = checkout();
        let config = Test262Config::parse(CONFIG).unwrap();
        let report = run_checkout(
            root.path(),
            &config,
            &InspectingAdapter,
            &RunOptions {
                parallel: false,
                verify_commit: false,
                ..RunOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.summary.total, 4);
        assert_eq!(report.summary.passed, 4);
        assert_eq!(report.summary.applicable_passed, 4);
        assert!(
            report
                .results
                .iter()
                .all(|result| result.result == ResultKind::Pass)
        );
    }
}
