use super::*;
use tachyon_bytecode::CompiledModule;

fn units(text: &str) -> Box<[u16]> {
    text.encode_utf16().collect::<Vec<_>>().into_boxed_slice()
}

fn compile(
    kind: DynamicFunctionKind,
    parameters: &[Box<[u16]>],
    body: &str,
) -> Result<CompiledModule, CompileError> {
    Compiler.compile_dynamic_function(
        SourceId::new(99),
        SourceName::new("dynamic-function"),
        kind,
        parameters,
        &units(body),
    )
}

#[test]
fn all_dynamic_function_kinds_compile() {
    for kind in [
        DynamicFunctionKind::Ordinary,
        DynamicFunctionKind::Generator,
        DynamicFunctionKind::Async,
        DynamicFunctionKind::AsyncGenerator,
    ] {
        assert!(compile(kind, &[units("value")], "return value;").is_ok());
    }
}

#[test]
fn parameters_and_body_cannot_complete_each_others_comments() {
    assert!(matches!(
        compile(DynamicFunctionKind::Ordinary, &[units("/*")], "*/ ) {"),
        Err(CompileError::Diagnostics(_))
    ));
}

#[test]
fn combined_early_errors_are_checked() {
    assert!(matches!(
        compile(
            DynamicFunctionKind::Ordinary,
            &[units("value")],
            "let value;"
        ),
        Err(CompileError::Diagnostics(_))
    ));
    assert!(matches!(
        compile(
            DynamicFunctionKind::Ordinary,
            &[units("value = 0")],
            "'use strict';"
        ),
        Err(CompileError::Diagnostics(_))
    ));
}

#[test]
fn lone_surrogate_is_not_lossily_replaced() {
    assert!(matches!(
        Compiler.compile_dynamic_function(
            SourceId::new(99),
            SourceName::new("dynamic-function"),
            DynamicFunctionKind::Ordinary,
            &[vec![0xd800].into_boxed_slice()],
            &[],
        ),
        Err(CompileError::MalformedDynamicFunctionUtf16)
    ));
}
