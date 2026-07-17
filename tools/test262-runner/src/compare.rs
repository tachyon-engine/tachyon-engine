use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Applicability, ResultKind, RunReport, SpecEdition, TestResult, VariantKind};

/// Stable identity of one independently counted Test262 execution.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TestKey {
    /// Checkout-relative Test262 path.
    pub path: Box<str>,
    /// Independently counted strictness/module variant.
    pub variant: VariantKind,
}

/// Semantic result and denominator classification needed for baseline comparison.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TestState {
    /// Semantic or infrastructure classification.
    pub result: ResultKind,
    /// Standardized denominator membership.
    pub applicability: Applicability,
    /// Minimum required ECMAScript edition.
    pub edition: SpecEdition,
}

/// Before/after transition for one stable test key.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResultChange {
    /// Stable identity shared by both reports.
    pub key: TestKey,
    /// Baseline state.
    pub before: TestState,
    /// New state.
    pub after: TestState,
}

/// Versioned fixed/broken/changed/add/remove comparison with explicit policy identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReportDiff {
    /// Diff JSON contract version.
    pub schema_version: u32,
    /// Baseline Test262 revision.
    pub base_commit: Box<str>,
    /// New Test262 revision.
    pub new_commit: Box<str>,
    /// Shared release-policy fingerprint.
    pub policy_sha256: Box<str>,
    /// Keys whose state is exactly unchanged.
    pub unchanged: u64,
    /// Non-pass to pass transitions.
    pub fixed: Vec<ResultChange>,
    /// Pass to non-pass transitions.
    pub broken: Vec<ResultChange>,
    /// Transitions between two distinct non-pass categories.
    pub changed: Vec<ResultChange>,
    /// Edition or applicability transitions, possibly overlapping semantic transitions.
    pub reclassified: Vec<ResultChange>,
    /// Keys present only in the new report.
    pub added: Vec<(TestKey, TestState)>,
    /// Keys present only in the baseline report.
    pub removed: Vec<(TestKey, TestState)>,
}

/// Reports cannot be compared when their schema, policy, or key uniqueness differs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompareError {
    /// Reports use incompatible JSON contracts.
    SchemaMismatch { base: u32, new: u32 },
    /// Release-target or feature-policy fingerprints differ.
    PolicyMismatch,
    /// One report contains the same path/variant more than once.
    DuplicateKey(TestKey),
}

/// Compares stable path/variant identities and never treats unsupported as absent from a baseline.
pub fn compare_reports(base: &RunReport, new: &RunReport) -> Result<ReportDiff, CompareError> {
    if base.schema_version != new.schema_version {
        return Err(CompareError::SchemaMismatch {
            base: base.schema_version,
            new: new.schema_version,
        });
    }
    if base.policy_sha256 != new.policy_sha256 {
        return Err(CompareError::PolicyMismatch);
    }
    let mut base_results = index_results(&base.results)?;
    let new_results = index_results(&new.results)?;
    let mut diff = ReportDiff {
        schema_version: 1,
        base_commit: base.test262_commit.clone(),
        new_commit: new.test262_commit.clone(),
        policy_sha256: base.policy_sha256.clone(),
        unchanged: 0,
        fixed: Vec::new(),
        broken: Vec::new(),
        changed: Vec::new(),
        reclassified: Vec::new(),
        added: Vec::new(),
        removed: Vec::new(),
    };
    for (key, after) in new_results {
        let Some(before) = base_results.remove(&key) else {
            diff.added.push((key, after));
            continue;
        };
        if before == after {
            diff.unchanged += 1;
            continue;
        }
        let change = ResultChange { key, before, after };
        if before.applicability != after.applicability || before.edition != after.edition {
            diff.reclassified.push(change.clone());
        }
        if before.result == after.result {
            continue;
        }
        if before.result != ResultKind::Pass && after.result == ResultKind::Pass {
            diff.fixed.push(change);
        } else if before.result == ResultKind::Pass && after.result != ResultKind::Pass {
            diff.broken.push(change);
        } else {
            diff.changed.push(change);
        }
    }
    diff.removed.extend(base_results);
    Ok(diff)
}

impl ReportDiff {
    /// Produces a compact deterministic summary; JSON retains every transition and diagnostic category.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        format!(
            "# Test262 comparison\n\n| Metric | Count |\n| --- | ---: |\n| Unchanged | {} |\n| Fixed | {} |\n| Broken | {} |\n| Changed | {} |\n| Reclassified | {} |\n| Added | {} |\n| Removed | {} |\n",
            self.unchanged,
            self.fixed.len(),
            self.broken.len(),
            self.changed.len(),
            self.reclassified.len(),
            self.added.len(),
            self.removed.len()
        )
    }
}

fn index_results(results: &[TestResult]) -> Result<BTreeMap<TestKey, TestState>, CompareError> {
    let mut indexed = BTreeMap::new();
    for result in results {
        let key = TestKey {
            path: result.path.clone(),
            variant: result.variant,
        };
        let state = TestState {
            result: result.result,
            applicability: result.applicability,
            edition: result.edition,
        };
        if indexed.insert(key.clone(), state).is_some() {
            return Err(CompareError::DuplicateKey(key));
        }
    }
    Ok(indexed)
}

impl core::fmt::Display for CompareError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::SchemaMismatch { base, new } => {
                write!(formatter, "report schema mismatch: base {base}, new {new}")
            }
            Self::PolicyMismatch => {
                formatter.write_str("report release-policy fingerprints differ")
            }
            Self::DuplicateKey(key) => write!(
                formatter,
                "duplicate Test262 result key: {} ({:?})",
                key.path, key.variant
            ),
        }
    }
}

impl std::error::Error for CompareError {}

#[cfg(test)]
mod tests {
    use crate::{
        Applicability, ResultKind, RunReport, RunSummary, SpecEdition, TestResult, VariantKind,
        compare_reports,
    };

    fn result(path: &str, result: ResultKind) -> TestResult {
        TestResult {
            path: path.into(),
            variant: VariantKind::Sloppy,
            edition: SpecEdition::Es5,
            applicability: Applicability::Applicable,
            applicability_reason: "fixture".into(),
            features: Vec::new(),
            expected_phase: None,
            result,
            source_sha256: "hash".into(),
            message: "".into(),
            stdout: "".into(),
            stderr: "".into(),
            backtrace: "".into(),
        }
    }

    fn report(results: Vec<TestResult>) -> RunReport {
        RunReport {
            schema_version: 2,
            test262_commit: "commit".into(),
            policy_sha256: "policy".into(),
            seed: None,
            summary: RunSummary::default(),
            results,
        }
    }

    #[test]
    /// Covers pass regressions, fixes, non-pass transitions, additions, removals, and stable Markdown counts.
    fn comparison_preserves_every_transition_category() {
        let base = report(vec![
            result("broken.js", ResultKind::Pass),
            result("fixed.js", ResultKind::SemanticFailure),
            result("changed.js", ResultKind::Unsupported),
            result("removed.js", ResultKind::Pass),
        ]);
        let new = report(vec![
            result("broken.js", ResultKind::ParseMismatch),
            result("fixed.js", ResultKind::Pass),
            result("changed.js", ResultKind::Timeout),
            result("added.js", ResultKind::Unsupported),
        ]);
        let diff = compare_reports(&base, &new).unwrap();
        assert_eq!(diff.fixed.len(), 1);
        assert_eq!(diff.broken.len(), 1);
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.removed.len(), 1);
        assert!(diff.to_markdown().contains("| Broken | 1 |"));
    }

    #[test]
    fn pure_denominator_reclassification_is_not_a_semantic_change() {
        let base = report(vec![result("test.js", ResultKind::Unsupported)]);
        let mut reclassified = result("test.js", ResultKind::Unsupported);
        reclassified.applicability = Applicability::NonApplicable;
        reclassified.edition = SpecEdition::EsNext;
        let diff = compare_reports(&base, &report(vec![reclassified])).unwrap();
        assert_eq!(diff.reclassified.len(), 1);
        assert!(diff.changed.is_empty());
        assert!(diff.fixed.is_empty());
        assert!(diff.broken.is_empty());
    }

    #[test]
    fn comparison_rejects_policy_drift_and_duplicate_result_keys() {
        let base = report(vec![result("test.js", ResultKind::Pass)]);
        let mut different_policy = report(vec![result("test.js", ResultKind::Pass)]);
        different_policy.policy_sha256 = "other-policy".into();
        assert_eq!(
            compare_reports(&base, &different_policy),
            Err(super::CompareError::PolicyMismatch)
        );

        let duplicate = report(vec![
            result("test.js", ResultKind::Pass),
            result("test.js", ResultKind::Unsupported),
        ]);
        assert!(matches!(
            compare_reports(&duplicate, &base),
            Err(super::CompareError::DuplicateKey(_))
        ));
    }
}
