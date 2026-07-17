use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{FeatureDisposition, Test262Config, TestFlag, TestMetadata};

/// Minimum published ECMAScript edition required by one Test262 variant.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SpecEdition {
    /// ECMAScript 5.1.
    Es5,
    /// ECMAScript 2015 / 6th edition.
    Es6,
    /// ECMAScript 2016 / 7th edition.
    Es7,
    /// ECMAScript 2017 / 8th edition.
    Es8,
    /// ECMAScript 2018 / 9th edition.
    Es9,
    /// ECMAScript 2019 / 10th edition.
    Es10,
    /// ECMAScript 2020 / 11th edition.
    Es11,
    /// ECMAScript 2021 / 12th edition.
    Es12,
    /// ECMAScript 2022 / 13th edition.
    Es13,
    /// ECMAScript 2023 / 14th edition.
    Es14,
    /// ECMAScript 2024 / 15th edition.
    Es15,
    /// ECMAScript 2025 / 16th edition.
    Es16,
    /// Proposal or post-release-target semantics.
    EsNext,
}

/// Status assigned by the pinned checkout's own `features.txt`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeatureStatus {
    /// Feature remains outside a published release target.
    Proposal,
    /// Feature appears in the registry's standardized section.
    Standardized,
}

/// Applicability to the standardized Tachyon 1.0 conformance denominator.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Applicability {
    /// Variant participates in the release conformance denominator.
    Applicable,
    /// Variant remains visible but is reported outside the denominator.
    NonApplicable,
}

/// Edition and denominator decision retained with every result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestClassification {
    /// Minimum required ECMAScript edition.
    pub edition: SpecEdition,
    /// Denominator membership.
    pub applicability: Applicability,
    /// Explicit policy explanation retained with each result.
    pub reason: Box<str>,
}

/// Parsed proposal/standardized status data from one verified checkout.
#[derive(Clone, Debug, Default)]
pub struct FeatureCatalog {
    statuses: BTreeMap<Box<str>, FeatureStatus>,
}

/// Feature metadata cannot be classified without guessing or hiding a test.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassificationError {
    /// Feature that could not be assigned without guessing.
    pub feature: Box<str>,
    /// Missing registry, edition, or policy fact.
    pub message: Box<str>,
}

impl FeatureCatalog {
    /// Parses Test262's line-oriented feature registry; later standardized entries replace stale proposal entries.
    pub fn parse(source: &str) -> Self {
        let mut catalog = Self::default();
        let mut section = None;
        for line in source.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("## Proposed language features") {
                section = Some(FeatureStatus::Proposal);
                continue;
            }
            if trimmed.starts_with("## Standard language features") {
                section = Some(FeatureStatus::Standardized);
                continue;
            }
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let feature = trimmed
                .split_once('#')
                .map_or(trimmed, |(name, _)| name)
                .trim();
            if let Some(status) = section.filter(|_| !feature.is_empty()) {
                catalog.statuses.insert(feature.into(), status);
            }
        }
        catalog
            .statuses
            .insert("IsHTMLDDA".into(), FeatureStatus::Standardized);
        catalog
            .statuses
            .insert("host-gc-required".into(), FeatureStatus::Standardized);
        catalog
    }

    #[must_use]
    pub fn status(&self, feature: &str) -> Option<FeatureStatus> {
        self.statuses.get(feature).copied()
    }

    /// Computes the maximum required edition and applies release/path/feature policy without dropping the test.
    pub fn classify(
        &self,
        metadata: &TestMetadata,
        path: &str,
        config: &Test262Config,
    ) -> Result<TestClassification, ClassificationError> {
        let mut edition = base_edition(metadata);
        let mut applicability = Applicability::Applicable;
        let mut reason: Box<str> = "release target".into();
        for feature in &metadata.features {
            let Some(status) = self.status(feature) else {
                return classify_unknown(feature, config, edition);
            };
            let feature_edition = match status {
                FeatureStatus::Proposal => SpecEdition::EsNext,
                FeatureStatus::Standardized => {
                    feature_edition(feature).ok_or_else(|| ClassificationError {
                        feature: feature.clone(),
                        message: "standardized feature has no checked-in edition assignment".into(),
                    })?
                }
            };
            edition = edition.max(feature_edition);
            let disposition = match status {
                FeatureStatus::Proposal => config.feature_policy.proposal,
                FeatureStatus::Standardized => config.feature_policy.standardized,
            };
            if disposition == FeatureDisposition::Error {
                return Err(ClassificationError {
                    feature: feature.clone(),
                    message: "feature policy rejects this status".into(),
                });
            }
            if disposition == FeatureDisposition::NonApplicable {
                applicability = Applicability::NonApplicable;
                reason = format!("feature `{feature}` is outside the release target").into();
            }
        }
        if edition > config.release_target.max_edition {
            applicability = Applicability::NonApplicable;
            reason = format!(
                "requires {edition}, release target ends at {}",
                config.release_target.max_edition
            )
            .into();
        }
        if !config.release_target.intl && path.starts_with("test/intl402/") {
            applicability = Applicability::NonApplicable;
            reason = "ECMA-402 disabled by release target".into();
        }
        if !config.release_target.annex_b && path.starts_with("test/annexB/") {
            applicability = Applicability::NonApplicable;
            reason = "Annex B disabled by release target".into();
        }
        Ok(TestClassification {
            edition,
            applicability,
            reason,
        })
    }
}

fn classify_unknown(
    feature: &str,
    config: &Test262Config,
    edition: SpecEdition,
) -> Result<TestClassification, ClassificationError> {
    match config.feature_policy.unknown {
        FeatureDisposition::Error => Err(ClassificationError {
            feature: feature.into(),
            message: "feature is absent from the pinned Test262 registry".into(),
        }),
        FeatureDisposition::NonApplicable => Ok(TestClassification {
            edition: SpecEdition::EsNext,
            applicability: Applicability::NonApplicable,
            reason: format!("unknown feature `{feature}`").into(),
        }),
        FeatureDisposition::Applicable => Err(ClassificationError {
            feature: feature.into(),
            message: format!(
                "unknown feature cannot be applicable without an edition assignment (base was {edition})"
            )
            .into(),
        }),
    }
}

fn base_edition(metadata: &TestMetadata) -> SpecEdition {
    if metadata.flags.contains(&TestFlag::Async) {
        SpecEdition::Es8
    } else if metadata.flags.contains(&TestFlag::Module)
        || metadata.esid.is_some()
        || metadata.es6id.is_some()
    {
        SpecEdition::Es6
    } else {
        SpecEdition::Es5
    }
}

impl core::fmt::Display for SpecEdition {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Es5 => formatter.write_str("ECMAScript 5.1"),
            Self::Es6 => formatter.write_str("ECMAScript 6"),
            Self::Es7 => formatter.write_str("ECMAScript 7"),
            Self::Es8 => formatter.write_str("ECMAScript 8"),
            Self::Es9 => formatter.write_str("ECMAScript 9"),
            Self::Es10 => formatter.write_str("ECMAScript 10"),
            Self::Es11 => formatter.write_str("ECMAScript 11"),
            Self::Es12 => formatter.write_str("ECMAScript 12"),
            Self::Es13 => formatter.write_str("ECMAScript 13"),
            Self::Es14 => formatter.write_str("ECMAScript 14"),
            Self::Es15 => formatter.write_str("ECMAScript 15"),
            Self::Es16 => formatter.write_str("ECMAScript 16"),
            Self::EsNext => formatter.write_str("ECMAScript Next"),
        }
    }
}

/// Edition assignments are factual compatibility data cross-checked against Boa's Test262 tester.
fn feature_edition(feature: &str) -> Option<SpecEdition> {
    Some(match feature {
        "caller" | "host-gc-required" => SpecEdition::Es5,
        "ArrayBuffer"
        | "Array.prototype.values"
        | "arrow-function"
        | "class"
        | "computed-property-names"
        | "const"
        | "cross-realm"
        | "DataView"
        | "DataView.prototype.getFloat16"
        | "DataView.prototype.getFloat32"
        | "DataView.prototype.getFloat64"
        | "DataView.prototype.getInt16"
        | "DataView.prototype.getInt32"
        | "DataView.prototype.getInt8"
        | "DataView.prototype.getUint16"
        | "DataView.prototype.getUint32"
        | "DataView.prototype.setUint8"
        | "default-parameters"
        | "destructuring-assignment"
        | "destructuring-binding"
        | "for-of"
        | "Float32Array"
        | "Float64Array"
        | "generators"
        | "Int8Array"
        | "Int16Array"
        | "Int32Array"
        | "let"
        | "Map"
        | "new.target"
        | "Object.is"
        | "Promise"
        | "Proxy"
        | "proxy-missing-checks"
        | "Reflect"
        | "Reflect.construct"
        | "Reflect.set"
        | "Reflect.setPrototypeOf"
        | "rest-parameters"
        | "Set"
        | "String.fromCodePoint"
        | "String.prototype.endsWith"
        | "String.prototype.includes"
        | "super"
        | "Symbol"
        | "Symbol.hasInstance"
        | "Symbol.isConcatSpreadable"
        | "Symbol.iterator"
        | "Symbol.match"
        | "Symbol.replace"
        | "Symbol.search"
        | "Symbol.species"
        | "Symbol.split"
        | "Symbol.toPrimitive"
        | "Symbol.toStringTag"
        | "Symbol.unscopables"
        | "tail-call-optimization"
        | "template"
        | "TypedArray"
        | "Uint8Array"
        | "Uint16Array"
        | "Uint32Array"
        | "Uint8ClampedArray"
        | "WeakMap"
        | "WeakSet"
        | "__proto__" => SpecEdition::Es6,
        "Array.prototype.includes" | "exponentiation" | "u180e" => SpecEdition::Es7,
        "async-functions"
        | "Atomics"
        | "intl-normative-optional"
        | "Intl.DateTimeFormat-dayPeriod"
        | "SharedArrayBuffer"
        | "__getter__"
        | "__setter__" => SpecEdition::Es8,
        "async-iteration"
        | "object-rest"
        | "object-spread"
        | "Promise.prototype.finally"
        | "regexp-dotall"
        | "regexp-lookbehind"
        | "regexp-named-groups"
        | "regexp-unicode-property-escapes"
        | "Symbol.asyncIterator"
        | "IsHTMLDDA" => SpecEdition::Es9,
        "Array.prototype.flat"
        | "Array.prototype.flatMap"
        | "json-superset"
        | "Object.fromEntries"
        | "optional-catch-binding"
        | "stable-array-sort"
        | "stable-typedarray-sort"
        | "string-trimming"
        | "String.prototype.trimEnd"
        | "String.prototype.trimStart"
        | "Symbol.prototype.description"
        | "well-formed-json-stringify" => SpecEdition::Es10,
        "BigInt"
        | "coalesce-expression"
        | "dynamic-import"
        | "export-star-as-namespace-from-module"
        | "for-in-order"
        | "globalThis"
        | "import.meta"
        | "Intl.NumberFormat-unified"
        | "Intl.RelativeTimeFormat"
        | "optional-chaining"
        | "Promise.allSettled"
        | "String.prototype.matchAll"
        | "Symbol.matchAll" => SpecEdition::Es11,
        "AggregateError"
        | "align-detached-buffer-semantics-with-web-reality"
        | "FinalizationRegistry"
        | "Intl.DateTimeFormat-datetimestyle"
        | "Intl.DateTimeFormat-formatRange"
        | "Intl.DateTimeFormat-fractionalSecondDigits"
        | "Intl.DisplayNames"
        | "Intl.ListFormat"
        | "Intl.Locale"
        | "logical-assignment-operators"
        | "numeric-separator-literal"
        | "Promise.any"
        | "String.prototype.replaceAll"
        | "WeakRef" => SpecEdition::Es12,
        "arbitrary-module-namespace-names"
        | "Array.prototype.at"
        | "class-fields-private"
        | "class-fields-private-in"
        | "class-fields-public"
        | "class-methods-private"
        | "class-static-block"
        | "class-static-fields-private"
        | "class-static-fields-public"
        | "class-static-methods-private"
        | "error-cause"
        | "Intl.DateTimeFormat-extend-timezonename"
        | "Intl.DisplayNames-v2"
        | "Intl.Segmenter"
        | "Object.hasOwn"
        | "regexp-match-indices"
        | "String.prototype.at"
        | "top-level-await"
        | "TypedArray.prototype.at" => SpecEdition::Es13,
        "array-find-from-last"
        | "change-array-by-copy"
        | "hashbang"
        | "Intl-enumeration"
        | "Intl.NumberFormat-v3"
        | "symbols-as-weakmap-keys" => SpecEdition::Es14,
        "Atomics.waitAsync"
        | "arraybuffer-transfer"
        | "array-grouping"
        | "promise-with-resolvers"
        | "regexp-v-flag"
        | "resizable-arraybuffer"
        | "String.prototype.isWellFormed"
        | "String.prototype.toWellFormed" => SpecEdition::Es15,
        "Float16Array"
        | "import-attributes"
        | "iterator-helpers"
        | "Intl.DurationFormat"
        | "json-modules"
        | "promise-try"
        | "RegExp.escape"
        | "regexp-duplicate-named-groups"
        | "regexp-modifiers"
        | "set-methods" => SpecEdition::Es16,
        "import-bytes"
        | "await-dictionary"
        | "Intl.Locale-info"
        | "legacy-regexp"
        | "import-defer"
        | "export-defer"
        | "import-text"
        | "canonical-tz"
        | "Temporal"
        | "ShadowRealm"
        | "decorators"
        | "Array.fromAsync"
        | "explicit-resource-management"
        | "Math.sumPrecise"
        | "source-phase-imports"
        | "source-phase-imports-module-source"
        | "Atomics.pause"
        | "immutable-arraybuffer"
        | "nonextensible-applies-to-private"
        | "joint-iteration"
        | "Error.isError"
        | "Intl.Era-monthcode"
        | "iterator-sequencing"
        | "upsert"
        | "json-parse-with-source"
        | "uint8array-base64"
        | "error-stack-accessor" => SpecEdition::EsNext,
        _ => return None,
    })
}

impl core::fmt::Display for ClassificationError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "cannot classify Test262 feature `{}`: {}",
            self.feature, self.message
        )
    }
}

impl std::error::Error for ClassificationError {}

#[cfg(test)]
mod tests {
    use crate::{Applicability, SpecEdition, Test262Config, TestMetadata};

    use super::{FeatureCatalog, FeatureStatus};

    const CONFIG: &str = include_str!("../../../test262_config.toml");

    #[test]
    /// Proves a stale proposal entry is replaced when the pinned registry later lists it as standardized.
    fn catalog_uses_the_latest_registry_section() {
        let catalog = FeatureCatalog::parse(
            "## Proposed language features\nBigInt\n\n## Standard language features\nBigInt\n",
        );
        assert_eq!(catalog.status("BigInt"), Some(FeatureStatus::Standardized));
    }

    #[test]
    /// Covers applicable published features, future editions, and the no-guess unknown-feature policy.
    fn classification_sets_edition_and_denominator_without_hiding_tests() {
        let config = Test262Config::parse(CONFIG).unwrap();
        let catalog = FeatureCatalog::parse(
            "## Proposed language features\nTemporal\n\n## Standard language features\nBigInt\n",
        );
        let bigint =
            TestMetadata::parse("/*---\ndescription: bigint\nfeatures: [BigInt]\n---*/").unwrap();
        let classified = catalog
            .classify(&bigint, "test/built-ins/BigInt/x.js", &config)
            .unwrap();
        assert_eq!(classified.edition, SpecEdition::Es11);
        assert_eq!(classified.applicability, Applicability::Applicable);

        let temporal =
            TestMetadata::parse("/*---\ndescription: temporal\nfeatures: [Temporal]\n---*/")
                .unwrap();
        let classified = catalog
            .classify(&temporal, "test/built-ins/Temporal/x.js", &config)
            .unwrap();
        assert_eq!(classified.edition, SpecEdition::EsNext);
        assert_eq!(classified.applicability, Applicability::NonApplicable);

        let unknown =
            TestMetadata::parse("/*---\ndescription: unknown\nfeatures: [future-unknown]\n---*/")
                .unwrap();
        assert!(catalog.classify(&unknown, "test/x.js", &config).is_err());
    }
}
