//! Pinned TC39 Signals proposal suite orchestration.

use std::{fs, path::Path, sync::Arc};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use test262_runner::{
    ComposedTest, EngineAdapter, EngineOutcome, ExecutionRequest, SourceUnit, TachyonAdapter,
    TestVariant, VariantKind,
};

const CONFIG_PATH: &str = "signals_suite.toml";

#[derive(Debug, Deserialize)]
struct SuiteConfig {
    schema_version: u32,
    proposal: ProposalPin,
    reference_suite: SourcePin,
    api_surface: Vec<Box<str>>,
    api_sha256: Box<str>,
    cases: Vec<SuiteCase>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SourcePin {
    repository: Box<str>,
    commit: Box<str>,
    content_sha256: Box<str>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ProposalPin {
    repository: Box<str>,
    commit: Box<str>,
    content_sha256: Box<str>,
    stage: u8,
}

#[derive(Debug, Deserialize)]
struct SuiteCase {
    id: Box<str>,
    path: Box<str>,
    source_sha256: Box<str>,
    upstream: Vec<Box<str>>,
}

#[derive(Debug, Serialize)]
struct SuiteReport<'a> {
    schema_version: u32,
    proposal: &'a ProposalPin,
    reference_suite: &'a SourcePin,
    api_sha256: &'a str,
    total: usize,
    passed: usize,
    results: Vec<CaseResult<'a>>,
}

#[derive(Debug, Serialize)]
struct CaseResult<'a> {
    id: &'a str,
    path: &'a str,
    source_sha256: &'a str,
    upstream: &'a [Box<str>],
    result: &'static str,
    message: Box<str>,
}

/// Validates provenance and content hashes, then executes every proposal fixture in-process.
pub(super) fn run(workspace: &Path) -> Result<(), String> {
    let config_source = fs::read_to_string(workspace.join(CONFIG_PATH))
        .map_err(|error| format!("failed to read {CONFIG_PATH}: {error}"))?;
    let config: SuiteConfig = toml::from_str(&config_source)
        .map_err(|error| format!("invalid {CONFIG_PATH}: {error}"))?;
    validate_config(&config)?;

    let results = config
        .cases
        .iter()
        .map(|case| run_case(workspace, case))
        .collect::<Result<Vec<_>, _>>()?;
    let passed = results
        .iter()
        .filter(|result| result.result == "pass")
        .count();
    let report = SuiteReport {
        schema_version: config.schema_version,
        proposal: &config.proposal,
        reference_suite: &config.reference_suite,
        api_sha256: &config.api_sha256,
        total: results.len(),
        passed,
        results,
    };
    serde_json::to_writer_pretty(std::io::stdout().lock(), &report)
        .map_err(|error| format!("failed to write Signals report: {error}"))?;
    println!();
    if passed == report.total {
        Ok(())
    } else {
        Err(format!(
            "pinned Signals suite failed: {passed}/{} cases passed",
            report.total
        ))
    }
}

/// Rejects stale pins or API manifests before any semantic case is executed.
fn validate_config(config: &SuiteConfig) -> Result<(), String> {
    if config.schema_version != 1 {
        return Err(format!(
            "unsupported Signals suite schema version {}",
            config.schema_version
        ));
    }
    validate_pin(
        "proposal",
        &config.proposal.repository,
        &config.proposal.commit,
        &config.proposal.content_sha256,
    )?;
    if config.proposal.stage != 1 {
        return Err(format!(
            "Signals proposal stage mismatch: pinned revision is Stage 1, manifest declares Stage {}",
            config.proposal.stage
        ));
    }
    validate_pin(
        "reference suite",
        &config.reference_suite.repository,
        &config.reference_suite.commit,
        &config.reference_suite.content_sha256,
    )?;
    let mut api = String::new();
    for entry in &config.api_surface {
        api.push_str(entry);
        api.push('\n');
    }
    let actual = digest(api.as_bytes());
    if actual != config.api_sha256.as_ref() {
        return Err(format!(
            "Signals API hash mismatch: expected {}, got {actual}",
            config.api_sha256
        ));
    }
    if config.cases.is_empty() {
        return Err("Signals suite must contain at least one case".to_owned());
    }
    Ok(())
}

fn validate_pin(
    name: &str,
    repository: &str,
    commit: &str,
    content_sha256: &str,
) -> Result<(), String> {
    if !is_lower_hex(commit, 40) || !is_lower_hex(content_sha256, 64) {
        return Err(format!("Signals {name} pin must use full lowercase hashes"));
    }
    if !repository.starts_with("https://github.com/") {
        return Err(format!(
            "Signals {name} repository must be an HTTPS GitHub URL"
        ));
    }
    Ok(())
}

/// Reads and verifies one fixture before presenting only owned bytes to the engine adapter.
fn run_case<'a>(workspace: &Path, case: &'a SuiteCase) -> Result<CaseResult<'a>, String> {
    if !is_lower_hex(&case.source_sha256, 64) {
        return Err(format!("Signals case `{}` has an invalid SHA-256", case.id));
    }
    let source = fs::read(workspace.join(case.path.as_ref()))
        .map_err(|error| format!("failed to read Signals case `{}`: {error}", case.id))?;
    let actual = digest(&source);
    if actual != case.source_sha256.as_ref() {
        return Err(format!(
            "Signals case `{}` SHA-256 mismatch: expected {}, got {actual}",
            case.id, case.source_sha256
        ));
    }
    let source = String::from_utf8(source)
        .map_err(|error| format!("Signals case `{}` is not UTF-8: {error}", case.id))?;
    let test = composed_test(&case.path, Arc::from(source));
    let response = TachyonAdapter.execute(ExecutionRequest {
        test: &test,
        can_block: false,
        is_module: false,
        is_async: false,
    });
    let (result, message) = describe_outcome(response.outcome);
    Ok(CaseResult {
        id: &case.id,
        path: &case.path,
        source_sha256: &case.source_sha256,
        upstream: &case.upstream,
        result,
        message,
    })
}

fn composed_test(path: &str, source: Arc<str>) -> ComposedTest {
    let source_sha256 = digest(source.as_bytes()).into_boxed_str();
    ComposedTest {
        variant: TestVariant {
            kind: VariantKind::Raw,
            is_async: false,
            can_block: false,
            use_harness: false,
        },
        preludes: Vec::new(),
        body: SourceUnit {
            name: path.into(),
            source,
        },
        source_sha256,
    }
}

fn describe_outcome(outcome: EngineOutcome) -> (&'static str, Box<str>) {
    match outcome {
        EngineOutcome::Completed => ("pass", "".into()),
        EngineOutcome::Error { message, .. }
        | EngineOutcome::Timeout { message }
        | EngineOutcome::Panic { message }
        | EngineOutcome::Crash { message }
        | EngineOutcome::HarnessFailure { message } => ("fail", message),
        EngineOutcome::Unsupported { reason } => ("fail", reason),
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
