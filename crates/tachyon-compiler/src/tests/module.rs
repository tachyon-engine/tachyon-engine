use super::*;
use tachyon_bytecode::{ModuleExportEntry, ModuleImportName};

fn compile_module(text: &str) -> tachyon_bytecode::CompiledModule {
    Compiler
        .compile(
            source(MediaType::Mjs, text),
            CompileOptions {
                source_mode: SourceMode::Module,
                ..CompileOptions::default()
            },
        )
        .expect("module should compile")
}

fn units(value: &str) -> Vec<u16> {
    value.encode_utf16().collect()
}

#[test]
/// Freezes the ParseModule partition and rewrites exports of imported bindings.
fn freezes_and_normalizes_static_module_semantics() {
    let module = compile_module(
        r#"
        import { source as local } from "dep" with { type: "json" };
        export { local as "public" };
        export * from "star";
        export * as ns from "namespace";
        export const own = 1;
        "#,
    );
    let stencil = module.module_stencil().expect("module stencil");

    assert_eq!(stencil.requested_modules().len(), 3);
    assert_eq!(
        stencil.requested_modules()[0].specifier.as_ref(),
        units("dep")
    );
    assert_eq!(stencil.requested_modules()[0].attributes.len(), 1);
    assert_eq!(stencil.imports().len(), 1);
    assert_eq!(stencil.imports()[0].local_name.as_ref(), "local");
    assert!(matches!(
        &stencil.exports()[0],
        ModuleExportEntry::Indirect {
            export_name,
            module_request,
            import_name: ModuleImportName::Name(import_name),
        } if export_name.as_ref() == units("public")
            && module_request.index() == 0
            && import_name.as_ref() == units("source")
    ));
    assert!(matches!(
        stencil.exports()[1],
        ModuleExportEntry::Star { .. }
    ));
    assert!(matches!(
        stencil.exports()[2],
        ModuleExportEntry::Indirect {
            import_name: ModuleImportName::Namespace,
            ..
        }
    ));
    assert!(
        stencil
            .local_bindings()
            .iter()
            .any(|name| name.as_ref() == "own")
    );
}

#[test]
/// Uses canonical attributes as part of source-ordered module request identity.
fn request_identity_includes_attributes_and_deduplicates_equal_requests() {
    let module = compile_module(
        r#"
        import "same" with { type: "json" };
        export { value } from "same" with { type: "json" };
        export { value as other } from "same" with { type: "javascript" };
        import "ordered" with { type: "json", mode: "strict" };
        export { value as ordered } from "ordered" with { mode: "strict", type: "json" };
        "#,
    );
    let stencil = module.module_stencil().expect("module stencil");

    assert_eq!(stencil.requested_modules().len(), 3);
    assert_eq!(
        stencil.requested_modules()[0].attributes[0].value.as_ref(),
        units("json")
    );
    assert_eq!(
        stencil.requested_modules()[1].attributes[0].value.as_ref(),
        units("javascript")
    );
    assert_eq!(stencil.requested_modules()[2].attributes.len(), 2);
}

#[test]
/// Counts Await in module code while excluding every nested function activation.
fn top_level_await_excludes_nested_async_functions() {
    let synchronous = compile_module("async function nested() { await 1; } export { nested };");
    assert!(
        !synchronous
            .module_stencil()
            .expect("module stencil")
            .has_top_level_await()
    );

    let asynchronous = compile_module("await 1;");
    assert!(
        asynchronous
            .module_stencil()
            .expect("module stencil")
            .has_top_level_await()
    );
}

#[test]
/// Prevents static module semantics from being attached to an ordinary script entry.
fn script_cannot_carry_a_module_stencil() {
    let script = Compiler
        .compile(
            source(MediaType::JavaScript, "1;"),
            CompileOptions::default(),
        )
        .expect("script should compile");
    let stencil =
        tachyon_bytecode::ModuleStencil::new(Vec::new(), Vec::new(), Vec::new(), Vec::new(), false)
            .expect("empty stencil is valid");

    assert!(matches!(
        script.with_module_stencil(stencil),
        Err(
            tachyon_bytecode::ModuleBuildError::ModuleStencilOnNonModuleEntry {
                kind: tachyon_bytecode::FunctionKind::Script,
            }
        )
    ));
}
