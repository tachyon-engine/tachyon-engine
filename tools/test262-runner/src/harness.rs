use std::{collections::BTreeMap, sync::Arc};

use sha2::{Digest, Sha256};

use crate::{TestMetadata, TestVariant, VariantKind};

/// One named in-memory source evaluated before or as the test body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceUnit {
    /// Stable diagnostic name, normally checkout-relative.
    pub name: Box<str>,
    /// Complete UTF-8 JavaScript source.
    pub source: Arc<str>,
}

/// Standard and test-specific Test262 harness files, keyed by checkout-relative harness name.
#[derive(Clone, Debug, Default)]
pub struct Harness {
    files: BTreeMap<Box<str>, Arc<str>>,
}

/// A fully selected test variant with deterministic source order and content hash.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComposedTest {
    /// Selected strictness/module and host policy.
    pub variant: TestVariant,
    /// Ordered standard and test-specific harness inputs.
    pub preludes: Vec<SourceUnit>,
    /// Test body after strict directive injection, if any.
    pub body: SourceUnit,
    /// SHA-256 over length-delimited names and all final source bytes.
    pub source_sha256: Box<str>,
}

/// A required standard or test-specific harness source is absent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HarnessError {
    /// A required checkout-relative harness source is absent.
    Missing { name: Box<str> },
    /// Include count plus standard harness entries overflowed `usize`.
    CapacityOverflow,
    /// A bounded source or prelude allocation failed.
    AllocationFailed,
}

impl Harness {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a caller-owned harness source. Duplicate names replace earlier data deterministically.
    pub fn insert(&mut self, name: impl Into<Box<str>>, source: impl Into<Arc<str>>) {
        self.files.insert(name.into(), source.into());
    }

    /// Composes standard harness files, declared includes, and the transformed test body in spec order.
    pub fn compose(
        &self,
        test_name: &str,
        source: Arc<str>,
        metadata: &TestMetadata,
        variant: TestVariant,
    ) -> Result<ComposedTest, HarnessError> {
        let standard_count = if variant.use_harness {
            2 + usize::from(variant.is_async)
        } else {
            0
        };
        let include_count = if variant.use_harness {
            metadata.includes.len()
        } else {
            0
        };
        let capacity = include_count
            .checked_add(standard_count)
            .ok_or(HarnessError::CapacityOverflow)?;
        let mut preludes = Vec::new();
        preludes
            .try_reserve_exact(capacity)
            .map_err(|_| HarnessError::AllocationFailed)?;
        if variant.use_harness {
            self.push_required(&mut preludes, "assert.js")?;
            self.push_required(&mut preludes, "sta.js")?;
            if variant.is_async {
                self.push_required(&mut preludes, "doneprintHandle.js")?;
            }
            for include in &metadata.includes {
                self.push_required(&mut preludes, include)?;
            }
        }
        let body_source = if variant.kind == VariantKind::Strict {
            let capacity = source
                .len()
                .checked_add(14)
                .ok_or(HarnessError::CapacityOverflow)?;
            let mut strict = String::new();
            strict
                .try_reserve_exact(capacity)
                .map_err(|_| HarnessError::AllocationFailed)?;
            strict.push_str("\"use strict\";\n");
            strict.push_str(&source);
            Arc::from(strict)
        } else {
            source
        };
        let body = SourceUnit {
            name: test_name.into(),
            source: body_source,
        };
        let source_sha256 = hash_sources(&preludes, &body).into_boxed_str();
        Ok(ComposedTest {
            variant,
            preludes,
            body,
            source_sha256,
        })
    }

    fn push_required(&self, output: &mut Vec<SourceUnit>, name: &str) -> Result<(), HarnessError> {
        let source = self
            .files
            .get(name)
            .ok_or_else(|| HarnessError::Missing { name: name.into() })?;
        output.push(SourceUnit {
            name: name.into(),
            source: source.clone(),
        });
        Ok(())
    }
}

/// Hashes length-delimited names and source bytes so concatenation boundaries cannot collide.
fn hash_sources(preludes: &[SourceUnit], body: &SourceUnit) -> String {
    let mut hasher = Sha256::new();
    for unit in preludes.iter().chain(core::iter::once(body)) {
        let name = unit.name.as_bytes();
        let source = unit.source.as_bytes();
        hasher.update((name.len() as u64).to_le_bytes());
        hasher.update(name);
        hasher.update((source.len() as u64).to_le_bytes());
        hasher.update(source);
    }
    format!("{:x}", hasher.finalize())
}

impl core::fmt::Display for HarnessError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Missing { name } => {
                write!(formatter, "missing Test262 harness source `{name}`")
            }
            Self::CapacityOverflow => formatter.write_str("Test262 source capacity overflow"),
            Self::AllocationFailed => formatter.write_str("Test262 source allocation failed"),
        }
    }
}

impl std::error::Error for HarnessError {}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{TestFlag, TestMetadata, VariantKind};

    use super::Harness;

    fn harness() -> Harness {
        let mut harness = Harness::new();
        harness.insert("assert.js", "assert harness");
        harness.insert("sta.js", "sta harness");
        harness.insert("doneprintHandle.js", "async harness");
        harness.insert("compareArray.js", "include harness");
        harness
    }

    #[test]
    /// Covers ordering, async setup, strict injection, and content-addressed reproducibility together.
    fn composition_orders_harness_and_hashes_final_sources() {
        let mut metadata = TestMetadata::parse("/*---\ndescription: x\nincludes: [compareArray.js]\nflags: [async, onlyStrict]\n---*/\nbody();").unwrap();
        metadata.flags = vec![TestFlag::Async, TestFlag::OnlyStrict];
        let variant = metadata.variants().unwrap().remove(0);
        let composed = harness()
            .compose("sample.js", Arc::from("body();"), &metadata, variant)
            .unwrap();
        assert_eq!(
            composed
                .preludes
                .iter()
                .map(|unit| &*unit.name)
                .collect::<Vec<_>>(),
            [
                "assert.js",
                "sta.js",
                "doneprintHandle.js",
                "compareArray.js"
            ]
        );
        assert_eq!(&*composed.body.source, "\"use strict\";\nbody();");
        assert_eq!(composed.source_sha256.len(), 64);
    }

    #[test]
    fn raw_variant_receives_no_implicit_harness() {
        let mut metadata =
            TestMetadata::parse("/*---\ndescription: x\nflags: [raw]\n---*/").unwrap();
        metadata.flags = vec![TestFlag::Raw];
        let variant = metadata.variants().unwrap().remove(0);
        assert_eq!(variant.kind, VariantKind::Raw);
        assert!(
            Harness::new()
                .compose("raw.js", Arc::from("raw();"), &metadata, variant)
                .unwrap()
                .preludes
                .is_empty()
        );
    }
}
