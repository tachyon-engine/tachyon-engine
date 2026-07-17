#![deny(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stderr,
    clippy::print_stdout,
    unsafe_op_in_unsafe_fn
)]
//! Oxc-facing compilation from caller-provided source text to owned bytecode.
//!
//! Source loading remains a host responsibility; this crate intentionally has no host I/O surface.

mod diagnostic;
mod parser;
mod source;

use std::sync::Arc;

pub use diagnostic::{Diagnostic, DiagnosticSeverity, RelatedDiagnosticSpan, SourceSpan};
pub use parser::{ParsedSource, ProgramKind};
pub use source::{CompileOptions, MediaType, SourceId, SourceMode, SourceName, SourceText};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompileError {
    SourceTooLarge {
        source_name: SourceName,
        byte_len: usize,
    },
    Diagnostics(Arc<[Diagnostic]>),
}

/// The stateless frontend boundary; source and all host configuration must be supplied per call.
#[derive(Clone, Copy, Debug, Default)]
pub struct Compiler;

impl Compiler {
    /// Parses source into owned frontend data and guarantees no Oxc arena or semantic value escapes this call.
    pub fn parse(
        &self,
        source: SourceText,
        options: CompileOptions,
    ) -> Result<ParsedSource, CompileError> {
        parser::parse(source, options)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use proptest::prelude::*;

    use super::*;

    fn source(media_type: MediaType, text: &str) -> SourceText {
        SourceText::new(
            SourceId::new(7),
            SourceName::new("embedded-input"),
            media_type,
            Arc::from(text),
        )
    }

    #[test]
    fn parser_copies_owned_information_before_dropping_oxc_arena() {
        let parsed = Compiler
            .parse(
                source(MediaType::TypeScript, "const answer: number = 42;"),
                CompileOptions::default(),
            )
            .unwrap();
        assert_eq!(parsed.source().name().as_str(), "embedded-input");
        assert_eq!(
            parsed.top_level_spans(),
            &[SourceSpan { start: 0, end: 26 }]
        );
        assert!(matches!(parsed.kind(), ProgramKind::Script));
        assert!(parsed.diagnostics().is_empty());
    }

    #[test]
    fn parser_supports_jsx_and_module_media_types() {
        let jsx = Compiler
            .parse(
                source(MediaType::Jsx, "const view = <main />;"),
                CompileOptions::default(),
            )
            .unwrap();
        assert!(matches!(jsx.kind(), ProgramKind::Module));
        let module = Compiler
            .parse(
                source(MediaType::Mts, "export const value: number = 1;"),
                CompileOptions::default(),
            )
            .unwrap();
        assert!(matches!(module.kind(), ProgramKind::Module));
    }

    #[test]
    fn parser_returns_owned_diagnostics_for_invalid_source() {
        let error = Compiler
            .parse(
                source(MediaType::JavaScript, "const = ;"),
                CompileOptions::default(),
            )
            .unwrap_err();
        let CompileError::Diagnostics(diagnostics) = error else {
            panic!("invalid JavaScript must report parser diagnostics");
        };
        assert!(!diagnostics.is_empty());
        assert_eq!(diagnostics[0].source_name.as_str(), "embedded-input");
        assert!(matches!(diagnostics[0].severity, DiagnosticSeverity::Error));
    }

    #[test]
    fn parser_honors_script_mode_for_module_syntax() {
        assert!(matches!(
            Compiler.parse(
                source(MediaType::JavaScript, "export {};"),
                CompileOptions {
                    source_mode: SourceMode::Script,
                },
            ),
            Err(CompileError::Diagnostics(_))
        ));
    }

    proptest! {
        #[test]
        fn arbitrary_utf8_input_never_escapes_the_frontend(
            characters in proptest::collection::vec(any::<char>(), 0..128),
        ) {
            let text: String = characters.into_iter().collect();
            let _ = Compiler.parse(
                source(MediaType::JavaScript, &text),
                CompileOptions::default(),
            );
        }
    }
}
