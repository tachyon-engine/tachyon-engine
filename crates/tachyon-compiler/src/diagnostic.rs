use std::sync::Arc;

use oxc::diagnostics::{OxcDiagnostic, Severity};

use crate::{SourceName, SourceText};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    Advice,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceSpan {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelatedDiagnosticSpan {
    pub span: SourceSpan,
    pub label: Option<Arc<str>>,
}

/// An owned compiler diagnostic with no Oxc or arena-backed data in its public representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub source_name: SourceName,
    pub severity: DiagnosticSeverity,
    pub message: Arc<str>,
    pub code: Option<Arc<str>>,
    pub primary: Option<RelatedDiagnosticSpan>,
    pub secondary: Arc<[RelatedDiagnosticSpan]>,
}

impl Diagnostic {
    #[must_use]
    pub(crate) fn parser_aborted(source: &SourceText) -> Self {
        Self {
            source_name: source.name().clone(),
            severity: DiagnosticSeverity::Error,
            message: Arc::from("Oxc parser aborted without a diagnostic"),
            code: None,
            primary: None,
            secondary: Arc::default(),
        }
    }

    /// Copies Oxc labels into source-bounded spans before the allocator and diagnostic values are dropped.
    #[must_use]
    pub(crate) fn from_oxc(diagnostic: OxcDiagnostic, source: &SourceText) -> Self {
        let mut primary = None;
        let mut secondary = Vec::new();
        for label in diagnostic.labels.as_deref().unwrap_or_default() {
            let Some(span) = source_span(label.offset(), label.len(), source.text().len()) else {
                continue;
            };
            let related = RelatedDiagnosticSpan {
                span,
                label: label.label().map(Arc::from),
            };
            if label.primary() && primary.is_none() {
                primary = Some(related);
            } else {
                secondary.push(related);
            }
        }
        if primary.is_none() && !secondary.is_empty() {
            primary = Some(secondary.remove(0));
        }
        let code = diagnostic
            .code
            .is_some()
            .then(|| Arc::from(diagnostic.code.to_string()));
        Self {
            source_name: source.name().clone(),
            severity: match diagnostic.severity {
                Severity::Advice => DiagnosticSeverity::Advice,
                Severity::Warning => DiagnosticSeverity::Warning,
                Severity::Error => DiagnosticSeverity::Error,
            },
            message: Arc::from(diagnostic.message.as_ref()),
            code,
            primary,
            secondary: secondary.into(),
        }
    }
}

/// Converts arbitrary diagnostic offsets defensively so malformed third-party diagnostics never escape bounds.
fn source_span(offset: usize, length: usize, source_length: usize) -> Option<SourceSpan> {
    let end = offset.checked_add(length)?;
    if end > source_length {
        return None;
    }
    Some(SourceSpan {
        start: u32::try_from(offset).ok()?,
        end: u32::try_from(end).ok()?,
    })
}
