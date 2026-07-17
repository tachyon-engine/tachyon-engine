use core::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::SpecEdition;

/// A pinned Test262 checkout and release-policy manifest.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Test262Config {
    /// JSON/TOML contract version for forward-compatible readers.
    pub schema_version: u32,
    /// Canonical upstream Git URL.
    pub repository: Box<str>,
    /// Full checkout SHA-1 required for every report.
    pub commit: Box<str>,
    /// Standards editions included in the Tachyon 1.0 target.
    pub release_target: ReleaseTarget,
    /// Applicability policy used to build the conformance denominator.
    pub feature_policy: FeaturePolicy,
}

/// Standards editions and normative suites included in the release target.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseTarget {
    /// Human-readable ECMA-262 edition identifier.
    pub ecma262: Box<str>,
    /// Human-readable ECMA-402 edition identifier.
    pub ecma402: Box<str>,
    /// Highest standardized ECMAScript edition included in the denominator.
    pub max_edition: SpecEdition,
    /// Whether normative-optional Annex B tests are applicable.
    pub annex_b: bool,
    /// Whether ECMA-402 tests are applicable.
    pub intl: bool,
}

/// Denominator policy for standardized, proposal, and unknown Test262 features.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FeaturePolicy {
    /// Policy for features standardized in the release target.
    pub standardized: FeatureDisposition,
    /// Policy for proposals outside the release target.
    pub proposal: FeatureDisposition,
    /// Policy when feature classification data is absent.
    pub unknown: FeatureDisposition,
    #[serde(default)]
    pub pinned_proposals: Vec<PinnedProposal>,
}

/// Whether tests in a feature class participate in conformance accounting.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FeatureDisposition {
    /// Count every matching variant in the denominator.
    Applicable,
    /// Report separately without counting it in the standardized denominator.
    NonApplicable,
    /// Reject the run until the manifest explicitly classifies the feature.
    Error,
}

/// A proposal tested outside the standardized Test262 denominator.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedProposal {
    /// Stable proposal identifier.
    pub name: Box<str>,
    /// Full upstream revision used by its separate suite.
    pub commit: Box<str>,
    /// Whether proposal tests enter the standardized Test262 denominator.
    pub included_in_test262_denominator: bool,
    /// Independent release-gate pass rate from zero through one.
    pub required_pass_rate: f64,
}

/// A malformed runner configuration with stable display text for CLI diagnostics.
#[derive(Debug)]
pub struct ConfigError(toml::de::Error);

impl Test262Config {
    /// Parses a complete manifest and rejects unknown fields so policy typos cannot silently drift.
    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        toml::from_str(source).map_err(ConfigError)
    }

    /// Checks fixed-width commit hashes and proposal rates before any checkout or suite run starts.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != 1 {
            return Err("unsupported test262 config schema version");
        }
        if !is_lower_hex_commit(&self.commit) {
            return Err("test262 commit must be a full lowercase hexadecimal SHA-1");
        }
        if self.release_target.max_edition == SpecEdition::EsNext {
            return Err("release target must name a published ECMAScript edition");
        }
        for proposal in &self.feature_policy.pinned_proposals {
            if !is_lower_hex_commit(&proposal.commit) {
                return Err("proposal commit must be a full lowercase hexadecimal SHA-1");
            }
            if !(0.0..=1.0).contains(&proposal.required_pass_rate) {
                return Err("proposal pass rate must be between zero and one");
            }
        }
        Ok(())
    }

    /// Hashes canonical struct-order JSON so reports cannot compare different denominator policies silently.
    pub fn policy_fingerprint(&self) -> Result<Box<str>, serde_json::Error> {
        let encoded = serde_json::to_vec(&(
            self.schema_version,
            &self.release_target,
            &self.feature_policy,
        ))?;
        Ok(format!("{:x}", Sha256::digest(encoded)).into_boxed_str())
    }
}

fn is_lower_hex_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid test262 config: {}", self.0)
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::{FeatureDisposition, Test262Config};

    const CONFIG: &str = include_str!("../../../test262_config.toml");

    #[test]
    fn repository_config_is_pinned_and_valid() {
        let config = Test262Config::parse(CONFIG).unwrap();
        config.validate().unwrap();
        assert_eq!(config.commit.len(), 40);
        assert_eq!(
            config.feature_policy.standardized,
            FeatureDisposition::Applicable
        );
        assert_eq!(config.feature_policy.pinned_proposals.len(), 1);
    }

    #[test]
    fn unknown_policy_fields_are_rejected() {
        let source = CONFIG.replace("intl = true", "intl = true\ntyop = false");
        assert!(Test262Config::parse(&source).is_err());
    }

    #[test]
    fn policy_fingerprint_excludes_checkout_commit_but_includes_denominator_rules() {
        let config = Test262Config::parse(CONFIG).unwrap();
        let mut other_commit = config.clone();
        other_commit.commit = "1111111111111111111111111111111111111111".into();
        assert_eq!(
            config.policy_fingerprint().unwrap(),
            other_commit.policy_fingerprint().unwrap()
        );
        let mut other_policy = config.clone();
        other_policy.release_target.annex_b = false;
        assert_ne!(
            config.policy_fingerprint().unwrap(),
            other_policy.policy_fingerprint().unwrap()
        );
    }
}
