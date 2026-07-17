use serde::{Deserialize, Serialize};

use crate::{ComposedTest, NegativeExpectation, Phase, TestMetadata, VariantKind};

/// Immutable in-memory input supplied to an engine implementation.
#[derive(Clone, Copy, Debug)]
pub struct ExecutionRequest<'a> {
    /// Fully composed in-memory source units.
    pub test: &'a ComposedTest,
    /// Whether the host may perform blocking Test262 operations.
    pub can_block: bool,
    /// Whether parse/link/evaluate must use module semantics.
    pub is_module: bool,
    /// Whether completion uses the asynchronous Test262 protocol.
    pub is_async: bool,
}

/// Phase-aware engine or harness outcome before Test262 expectation classification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineOutcome {
    /// Evaluation completed without an uncaught error.
    Completed,
    /// ECMAScript evaluation failed with a phase and constructor name.
    Error {
        /// Phase that produced the error.
        phase: Phase,
        /// ECMAScript error constructor name.
        error_type: Box<str>,
        /// Adapter diagnostic text.
        message: Box<str>,
    },
    /// Configured execution deadline or fuel policy expired.
    Timeout {
        /// Timeout diagnostic.
        message: Box<str>,
    },
    /// In-process engine unwound through the adapter boundary.
    Panic {
        /// Panic payload rendered without discarding the result category.
        message: Box<str>,
    },
    /// Child process terminated abnormally.
    Crash {
        /// Exit-status or signal diagnostic.
        message: Box<str>,
    },
    /// The adapter cannot yet execute this semantic surface.
    Unsupported {
        /// Explicit unsupported reason retained in the denominator.
        reason: Box<str>,
    },
    /// A required harness source or host binding failed before the test body.
    HarnessFailure {
        /// Harness setup diagnostic.
        message: Box<str>,
    },
}

/// Captured diagnostics retained for every in-process or child-process outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineResponse {
    /// Phase-aware semantic or infrastructure outcome.
    pub outcome: EngineOutcome,
    /// Captured standard output.
    pub stdout: Box<str>,
    /// Captured standard error.
    pub stderr: Box<str>,
    /// Trimmed panic or crash backtrace when available.
    pub backtrace: Box<str>,
}

impl EngineResponse {
    #[must_use]
    pub fn new(outcome: EngineOutcome) -> Self {
        Self {
            outcome,
            stdout: "".into(),
            stderr: "".into(),
            backtrace: "".into(),
        }
    }
}

/// An engine boundary usable by Tachyon, external processes, or deterministic fixtures.
pub trait EngineAdapter: Sync {
    /// Executes one variant without reading source or harness files itself.
    fn execute(&self, request: ExecutionRequest<'_>) -> EngineResponse;
}

/// Every result category remains distinct in JSON and conformance accounting.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResultKind {
    /// Actual outcome matched the positive or negative expectation.
    Pass,
    /// Runtime/resolution semantics or error type did not match.
    SemanticFailure,
    /// Parse acceptance/rejection or parse-phase error type did not match.
    ParseMismatch,
    /// Execution exceeded its bound.
    Timeout,
    /// In-process panic crossed the adapter boundary.
    Panic,
    /// External engine process terminated abnormally.
    Crash,
    /// Adapter explicitly lacks the requested behavior.
    Unsupported,
    /// Standard or declared harness setup failed.
    HarnessFailure,
}

/// Stable per-variant result record. Diagnostics are retained rather than printed by engine code.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TestResult {
    /// Checkout-relative test path.
    pub path: Box<str>,
    /// Independently counted execution variant.
    pub variant: VariantKind,
    /// Upstream feature classifications.
    pub features: Vec<Box<str>>,
    /// Negative phase, or `None` for a positive test.
    pub expected_phase: Option<Phase>,
    /// Non-collapsed final classification.
    pub result: ResultKind,
    /// Hash of the exact harness and body inputs.
    pub source_sha256: Box<str>,
    /// Primary classification diagnostic.
    pub message: Box<str>,
    /// Captured adapter standard output.
    pub stdout: Box<str>,
    /// Captured adapter standard error.
    pub stderr: Box<str>,
    /// Trimmed adapter backtrace.
    pub backtrace: Box<str>,
}

/// A deterministic adapter used before the VM supports the requested Test262 semantics.
#[derive(Clone, Debug)]
pub struct StubAdapter {
    response: EngineResponse,
}

impl StubAdapter {
    #[must_use]
    pub fn new(outcome: EngineOutcome) -> Self {
        Self {
            response: EngineResponse::new(outcome),
        }
    }

    #[must_use]
    pub fn from_response(response: EngineResponse) -> Self {
        Self { response }
    }

    #[must_use]
    pub fn unsupported() -> Self {
        Self::new(EngineOutcome::Unsupported {
            reason: "stub adapter does not execute JavaScript".into(),
        })
    }
}

impl EngineAdapter for StubAdapter {
    fn execute(&self, _request: ExecutionRequest<'_>) -> EngineResponse {
        self.response.clone()
    }
}

/// Executes one composed variant and classifies actual phase/type against its negative expectation.
pub fn run_test(
    adapter: &dyn EngineAdapter,
    path: &str,
    metadata: &TestMetadata,
    test: &ComposedTest,
) -> TestResult {
    let request = ExecutionRequest {
        test,
        can_block: test.variant.can_block,
        is_module: test.variant.kind == VariantKind::Module,
        is_async: test.variant.is_async,
    };
    let response = adapter.execute(request);
    let (result, message) = classify(metadata.negative.as_ref(), response.outcome);
    TestResult {
        path: path.into(),
        variant: test.variant.kind,
        features: metadata.features.clone(),
        expected_phase: metadata.negative.as_ref().map(|negative| negative.phase),
        result,
        source_sha256: test.source_sha256.clone(),
        message,
        stdout: response.stdout,
        stderr: response.stderr,
        backtrace: response.backtrace,
    }
}

/// Maps all engine outcomes without collapsing parse, semantic, infrastructure, or process failures.
fn classify(
    expected: Option<&NegativeExpectation>,
    actual: EngineOutcome,
) -> (ResultKind, Box<str>) {
    match actual {
        EngineOutcome::Completed => match expected {
            None => empty_result(ResultKind::Pass),
            Some(expected) => message_result(
                mismatch_kind(expected.phase),
                format!(
                    "expected {} during {:?}, but execution completed",
                    expected.error_type, expected.phase
                ),
            ),
        },
        EngineOutcome::Error {
            phase,
            error_type,
            message,
        } => match expected {
            Some(expected) if expected.phase == phase && expected.error_type == error_type => {
                message_result(ResultKind::Pass, message)
            }
            Some(expected) => message_result(
                if expected.phase == Phase::Parse || phase == Phase::Parse {
                    ResultKind::ParseMismatch
                } else {
                    ResultKind::SemanticFailure
                },
                format!(
                    "expected {} during {:?}, got {} during {:?}: {}",
                    expected.error_type, expected.phase, error_type, phase, message
                ),
            ),
            None => message_result(
                if phase == Phase::Parse {
                    ResultKind::ParseMismatch
                } else {
                    ResultKind::SemanticFailure
                },
                format!("unexpected {error_type} during {phase:?}: {message}"),
            ),
        },
        EngineOutcome::Timeout { message } => message_result(ResultKind::Timeout, message),
        EngineOutcome::Panic { message } => message_result(ResultKind::Panic, message),
        EngineOutcome::Crash { message } => message_result(ResultKind::Crash, message),
        EngineOutcome::Unsupported { reason } => message_result(ResultKind::Unsupported, reason),
        EngineOutcome::HarnessFailure { message } => {
            message_result(ResultKind::HarnessFailure, message)
        }
    }
}

fn mismatch_kind(phase: Phase) -> ResultKind {
    if phase == Phase::Parse {
        ResultKind::ParseMismatch
    } else {
        ResultKind::SemanticFailure
    }
}

fn empty_result(kind: ResultKind) -> (ResultKind, Box<str>) {
    (kind, "".into())
}

fn message_result(kind: ResultKind, message: impl Into<Box<str>>) -> (ResultKind, Box<str>) {
    (kind, message.into())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{Harness, Phase, TestMetadata};

    use super::{EngineOutcome, EngineResponse, ResultKind, StubAdapter, run_test};

    fn run(source: &str, outcome: EngineOutcome) -> ResultKind {
        let metadata = TestMetadata::parse(source).unwrap();
        let variant = metadata.variants().unwrap().remove(0);
        let mut harness = Harness::new();
        harness.insert("assert.js", "");
        harness.insert("sta.js", "");
        let composed = harness
            .compose("test.js", Arc::from(source), &metadata, variant)
            .unwrap();
        run_test(
            &StubAdapter::new(outcome),
            "test/language/test.js",
            &metadata,
            &composed,
        )
        .result
    }

    #[test]
    /// Proves positive and negative results use both exact phase and exact error constructor.
    fn positive_and_phase_accurate_negative_tests_pass() {
        assert_eq!(
            run(
                "/*---\ndescription: positive\n---*/",
                EngineOutcome::Completed
            ),
            ResultKind::Pass
        );
        let negative =
            "/*---\ndescription: negative\nnegative:\n  phase: parse\n  type: SyntaxError\n---*/";
        assert_eq!(
            run(
                negative,
                EngineOutcome::Error {
                    phase: Phase::Parse,
                    error_type: "SyntaxError".into(),
                    message: "expected".into(),
                }
            ),
            ResultKind::Pass
        );
    }

    #[test]
    fn wrong_negative_phase_is_not_counted_as_a_pass() {
        let negative =
            "/*---\ndescription: negative\nnegative:\n  phase: runtime\n  type: TypeError\n---*/";
        assert_eq!(
            run(
                negative,
                EngineOutcome::Error {
                    phase: Phase::Parse,
                    error_type: "SyntaxError".into(),
                    message: "wrong phase".into(),
                }
            ),
            ResultKind::ParseMismatch
        );
    }

    #[test]
    fn infrastructure_outcomes_remain_distinct() {
        let positive = "/*---\ndescription: positive\n---*/";
        assert_eq!(
            run(
                positive,
                EngineOutcome::Unsupported {
                    reason: "stub".into(),
                }
            ),
            ResultKind::Unsupported
        );
        assert_eq!(
            run(
                positive,
                EngineOutcome::HarnessFailure {
                    message: "bad include".into(),
                }
            ),
            ResultKind::HarnessFailure
        );
    }

    #[test]
    /// Ensures diagnostics survive even when the semantic result itself is successful.
    fn adapter_diagnostics_are_retained_for_every_result_kind() {
        let source = "/*---\ndescription: positive\n---*/";
        let metadata = TestMetadata::parse(source).unwrap();
        let variant = metadata.variants().unwrap().remove(0);
        let mut harness = Harness::new();
        harness.insert("assert.js", "");
        harness.insert("sta.js", "");
        let composed = harness
            .compose("test.js", Arc::from(source), &metadata, variant)
            .unwrap();
        let adapter = StubAdapter::from_response(EngineResponse {
            outcome: EngineOutcome::Completed,
            stdout: "captured stdout".into(),
            stderr: "captured stderr".into(),
            backtrace: "captured backtrace".into(),
        });
        let result = run_test(&adapter, "test.js", &metadata, &composed);
        assert_eq!(result.stdout.as_ref(), "captured stdout");
        assert_eq!(result.stderr.as_ref(), "captured stderr");
        assert_eq!(result.backtrace.as_ref(), "captured backtrace");
    }
}
