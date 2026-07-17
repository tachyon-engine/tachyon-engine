#![deny(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stderr,
    clippy::print_stdout,
    unsafe_op_in_unsafe_fn
)]
//! Stable Rust facade for embedding the Tachyon ECMAScript engine.
//!
//! Hosts provide source bytes, module loading, clocks, entropy, and event-loop integration.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};
    use tachyon_vm::{ExecutionBudget, Isolate, RunOutcome};

    #[test]
    fn source_to_verified_module_to_int32_result() {
        let source = SourceText::new(
            SourceId::new(0),
            SourceName::new("embedded-input"),
            MediaType::JavaScript,
            Arc::from("1 + 2;"),
        );
        let module = Compiler.compile(source, CompileOptions::default()).unwrap();
        let outcome = Isolate::default()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    }
}
