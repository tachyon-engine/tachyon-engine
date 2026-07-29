use super::*;
use tachyon_bytecode::{BindingLocation, FunctionKind, ModuleExportEntry, ModuleImportName};

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
/// Materializes anonymous default values in the hidden immutable module cell.
fn anonymous_default_expression_uses_the_synthetic_binding() {
    let module = compile_module("export default 42;");
    let stencil = module.module_stencil().expect("module stencil");
    assert_eq!(stencil.local_bindings(), &[Arc::from("*default*")]);
    assert!(matches!(
        &stencil.exports()[0],
        ModuleExportEntry::Local {
            export_name,
            local_name,
        } if export_name.as_ref() == units("default") && local_name.as_ref() == "*default*"
    ));
    let entry = module
        .function(tachyon_bytecode::FunctionId::new(0))
        .unwrap();
    assert_eq!(entry.environment_slots().len(), 1);
    assert_eq!(entry.environment_slots()[0].name.as_ref(), "*default*");
    assert!(!entry.environment_slots()[0].mutable);
    assert!(!entry.environment_slots()[0].initialized);
    assert!(entry.binding_plan().iter().any(|binding| {
        binding.name.as_ref() == "*default*"
            && binding.location == BindingLocation::ModuleCell { slot: 0 }
            && !binding.mutable
    }));
}

#[test]
/// Keeps anonymous function/class default names public while storing their hidden local name.
fn anonymous_default_declarations_use_default_for_function_names() {
    let cases = [
        ("export default function () {}", FunctionKind::Ordinary),
        ("export default async function () {}", FunctionKind::Async),
        ("export default function* () {}", FunctionKind::Generator),
        (
            "export default async function* () {}",
            FunctionKind::AsyncGenerator,
        ),
    ];
    for (source_text, kind) in cases {
        let module = compile_module(source_text);
        let stencil = module.module_stencil().expect("module stencil");
        assert_eq!(stencil.local_bindings(), &[Arc::from("*default*")]);
        let function = module
            .function(tachyon_bytecode::FunctionId::new(1))
            .unwrap();
        assert_eq!(function.kind(), kind);
        let name_scope = function.layout().name_scope.expect("inferred name");
        assert_eq!(
            module.scope_names()[name_scope as usize].as_ref(),
            "default"
        );
        let entry = module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap();
        assert_eq!(entry.environment_slots()[0].name.as_ref(), "*default*");
        assert!(entry.environment_slots()[0].mutable);
        assert!(!entry.environment_slots()[0].initialized);
    }
}

#[test]
/// Applies ExportDeclaration NamedEvaluation to anonymous arrow and class defaults.
fn anonymous_default_expression_infers_arrow_and_class_names() {
    let arrow = compile_module("export default () => 1;");
    let arrow_function = arrow
        .function(tachyon_bytecode::FunctionId::new(1))
        .unwrap();
    let arrow_name = arrow_function.layout().name_scope.expect("arrow name");
    assert_eq!(arrow.scope_names()[arrow_name as usize].as_ref(), "default");

    let class = compile_module("export default class {};");
    let class_constructor = class
        .function(tachyon_bytecode::FunctionId::new(1))
        .unwrap();
    let class_name = class_constructor
        .layout()
        .name_scope
        .expect("class constructor name");
    assert_eq!(class.scope_names()[class_name as usize].as_ref(), "default");
    assert!(
        class
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap()
            .environment_slots()
            .iter()
            .any(|slot| slot.name.as_ref() == "*default*" && !slot.mutable)
    );
}

#[test]
/// Rebases module cells across a named class environment surrounding suspended heritage code.
fn module_binding_depth_accounts_for_class_heritage_environment() {
    let module = compile_module(
        "function fn() { return function() {}; } export class C extends fn(await 1) {}",
    );
    let entry = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(entry.contains("EnterClassEnvironment slots=1"));
    assert!(entry.contains("LoadEnvironment r"));
    assert!(entry.contains("depth=1, slot=1"));
    assert!(entry.contains("Await"));
}

#[test]
/// Resolves one module cell from both sides of a temporary named-class environment.
fn module_binding_depth_is_not_cached_from_first_nested_use() {
    let module =
        compile_module("let value = 1; export default class C extends (value, Object) {} ; value;");
    let entry = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(entry.contains("EnterClassEnvironment slots=1"));
    assert!(entry.contains("depth=1, slot=1"));
    assert!(entry.contains("depth=0, slot=1"));
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
