use std::sync::Arc;

use oxc::{
    allocator::Allocator,
    parser::Parser,
    semantic::SemanticBuilder,
    span::{GetSpan, SourceType},
};

use crate::{
    CompileError, CompileOptions, Diagnostic, DiagnosticSeverity, MediaType, SourceMode,
    SourceSpan, SourceText,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgramKind {
    Script,
    Module,
    CommonJs,
}

/// An owned parse result that deliberately retains only data needed by the HIR lowering stage.
#[derive(Clone, Debug)]
pub struct ParsedSource {
    source: SourceText,
    kind: ProgramKind,
    top_level_spans: Arc<[SourceSpan]>,
    diagnostics: Arc<[Diagnostic]>,
}

impl ParsedSource {
    #[must_use]
    pub fn source(&self) -> &SourceText {
        &self.source
    }

    #[must_use]
    pub const fn kind(&self) -> ProgramKind {
        self.kind
    }

    #[must_use]
    pub fn top_level_spans(&self) -> &[SourceSpan] {
        &self.top_level_spans
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

/// Parses and semantically validates a caller-provided source buffer, then drops every Oxc arena value.
pub(crate) fn parse(
    source: SourceText,
    options: CompileOptions,
) -> Result<ParsedSource, CompileError> {
    let (parsed_source, ()) = parse_with(source, options, |_, _, _, _| Ok(()))?;
    Ok(parsed_source)
}

/// Parses and semantically validates source, then invokes a lowering closure while the Oxc arena is alive.
pub(crate) fn parse_with<T>(
    source: SourceText,
    options: CompileOptions,
    lower: impl FnOnce(
        &oxc::ast::ast::Program<'_>,
        ProgramKind,
        &SourceText,
        &oxc::semantic::Semantic<'_>,
    ) -> Result<T, CompileError>,
) -> Result<(ParsedSource, T), CompileError> {
    if source.text().len() > u32::MAX as usize {
        return Err(CompileError::SourceTooLarge {
            source_name: source.name().clone(),
            byte_len: source.text().len(),
        });
    }
    let allocator = Allocator::default();
    let source_type = source_type(source.media_type(), options.source_mode);
    let parsed = Parser::new(&allocator, source.text(), source_type).parse();
    let mut diagnostics: Vec<_> = parsed
        .diagnostics
        .into_iter()
        .map(|diagnostic| Diagnostic::from_oxc(diagnostic, &source))
        .collect();
    if parsed.panicked && diagnostics.is_empty() {
        diagnostics.push(Diagnostic::parser_aborted(&source));
    }
    if has_errors(&diagnostics) {
        return Err(CompileError::Diagnostics(diagnostics.into()));
    }

    let module_syntax = parsed.module_record.has_module_syntax;
    let kind = program_kind(parsed.program.source_type, module_syntax);
    let top_level_spans = parsed
        .program
        .body
        .iter()
        .map(|statement| source_span(statement.span()))
        .collect::<Vec<_>>();
    let semantic = SemanticBuilder::new()
        .with_check_syntax_error(true)
        .build(&parsed.program);
    diagnostics.extend(
        semantic
            .diagnostics
            .into_iter()
            .map(|diagnostic| Diagnostic::from_oxc(diagnostic, &source)),
    );
    if has_errors(&diagnostics) {
        return Err(CompileError::Diagnostics(diagnostics.into()));
    }

    let lowered = lower(&parsed.program, kind, &source, &semantic.semantic)?;
    Ok((
        ParsedSource {
            source,
            kind,
            top_level_spans: top_level_spans.into(),
            diagnostics: diagnostics.into(),
        },
        lowered,
    ))
}

fn source_type(media_type: MediaType, mode: SourceMode) -> SourceType {
    let source_type = match media_type {
        MediaType::JavaScript => SourceType::unambiguous(),
        MediaType::Jsx => SourceType::jsx(),
        MediaType::TypeScript => SourceType::ts(),
        MediaType::Tsx => SourceType::tsx(),
        MediaType::Mjs => SourceType::mjs(),
        MediaType::Cjs => SourceType::cjs(),
        MediaType::Mts | MediaType::Cts => SourceType::ts().with_module(true),
    };
    match mode {
        SourceMode::Auto => source_type,
        SourceMode::Script => source_type.with_script(true),
        SourceMode::Module => source_type.with_module(true),
    }
}

fn program_kind(source_type: SourceType, has_module_syntax: bool) -> ProgramKind {
    if source_type.is_commonjs() {
        ProgramKind::CommonJs
    } else if source_type.is_module() || has_module_syntax {
        ProgramKind::Module
    } else {
        ProgramKind::Script
    }
}

fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
}

fn source_span(span: oxc::span::Span) -> SourceSpan {
    SourceSpan {
        start: span.start,
        end: span.end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_types_match_the_oxc_source_type_contract() {
        let javascript = source_type(MediaType::JavaScript, SourceMode::Auto);
        assert!(javascript.is_javascript() && javascript.is_unambiguous());
        let jsx = source_type(MediaType::Jsx, SourceMode::Auto);
        assert!(jsx.is_javascript() && jsx.is_jsx() && jsx.is_module());
        let typescript = source_type(MediaType::TypeScript, SourceMode::Auto);
        assert!(typescript.is_typescript() && typescript.is_unambiguous());
        let tsx = source_type(MediaType::Tsx, SourceMode::Auto);
        assert!(tsx.is_typescript() && tsx.is_jsx() && tsx.is_unambiguous());
        assert!(source_type(MediaType::Mjs, SourceMode::Auto).is_module());
        assert!(source_type(MediaType::Cjs, SourceMode::Auto).is_commonjs());
        for media_type in [MediaType::Mts, MediaType::Cts] {
            let source_type = source_type(media_type, SourceMode::Auto);
            assert!(source_type.is_typescript() && source_type.is_module());
        }
    }
}
