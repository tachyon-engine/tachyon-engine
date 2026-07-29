use super::*;
use crate::module::*;
use tachyon_compiler::{
    CompileOptions, Compiler, MediaType, SourceId, SourceMode, SourceName, SourceText,
};

fn specifier(value: &str) -> ModuleIdentity {
    ModuleIdentity::try_new(value).expect("test module specifier allocates")
}

fn binding(value: &str) -> ModuleBindingName {
    ModuleBindingName::try_new(value).expect("test module binding allocates")
}

fn public_name(value: &str) -> ModuleExportName {
    ModuleExportName::try_new(value).expect("test module export name allocates")
}

fn request_list(values: &[&str]) -> Box<[ModuleIdentity]> {
    values
        .iter()
        .map(|value| specifier(value))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn binding_list(values: &[&str]) -> Box<[ModuleBindingName]> {
    values
        .iter()
        .map(|value| binding(value))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn local_export(export_name: &str, local_name: &str) -> ExportEntry {
    local_export_name(public_name(export_name), local_name)
}

fn local_export_name(export_name: ModuleExportName, local_name: &str) -> ExportEntry {
    ExportEntry::Local {
        export_name,
        local_name: binding(local_name),
    }
}

fn indirect_export(export_name: &str, module_request: &str, import_name: &str) -> ExportEntry {
    ExportEntry::Indirect {
        export_name: public_name(export_name),
        module_request: specifier(module_request),
        import_name: ModuleImportName::Name(public_name(import_name)),
    }
}

fn named_import(module_request: &str, import_name: &str, local_name: &str) -> ImportEntry {
    named_import_name(module_request, public_name(import_name), local_name)
}

fn named_import_name(
    module_request: &str,
    import_name: ModuleExportName,
    local_name: &str,
) -> ImportEntry {
    ImportEntry::new(
        specifier(module_request),
        ModuleImportName::Name(import_name),
        binding(local_name),
    )
}

fn star_export(module_request: &str) -> ExportEntry {
    ExportEntry::Star {
        module_request: specifier(module_request),
    }
}

fn namespace_export(export_name: &str, module_request: &str) -> ExportEntry {
    ExportEntry::Indirect {
        export_name: public_name(export_name),
        module_request: specifier(module_request),
        import_name: ModuleImportName::Namespace,
    }
}

fn namespace_import(module_request: &str, local_name: &str) -> ImportEntry {
    ImportEntry::new(
        specifier(module_request),
        ModuleImportName::Namespace,
        binding(local_name),
    )
}

fn record(
    specifier_name: &str,
    requests: &[&str],
    imports: Vec<ImportEntry>,
    exports: Vec<ExportEntry>,
    locals: &[&str],
) -> ModuleRecordInit {
    ModuleRecordInit {
        specifier: specifier(specifier_name),
        requested_modules: request_list(requests),
        imports: imports.into_boxed_slice(),
        exports: exports.into_boxed_slice(),
        local_bindings: binding_list(locals),
        has_top_level_await: false,
    }
}

fn test_graph() -> ModuleGraph {
    ModuleGraph::try_new(ModuleLimits::new(64, 128, 512)).expect("test module graph allocates")
}

#[test]
/// A linked import must observe every later update to the exporter's original cell.
fn module_imports_share_the_exporting_live_binding_cell() {
    let mut graph = test_graph();
    let exporter = graph
        .insert(record(
            "memory:a",
            &[],
            vec![],
            vec![local_export("x", "x")],
            &["x"],
        ))
        .unwrap();
    let importer = graph
        .insert(record(
            "memory:b",
            &["memory:a"],
            vec![named_import("memory:a", "x", "seen")],
            vec![],
            &[],
        ))
        .unwrap();

    assert_eq!(
        graph.read_binding(exporter, "x"),
        Err(ModuleError::UninitializedBinding)
    );
    graph
        .write_binding(exporter, "x", Value::from_i32(1))
        .unwrap();
    graph.link(importer).unwrap();
    assert_eq!(
        graph.read_binding(importer, "seen").unwrap().as_i32(),
        Some(1)
    );
    assert!(
        graph.record(importer).unwrap().imports()[0]
            .resolved()
            .is_some()
    );

    graph
        .write_binding(exporter, "x", Value::from_i32(2))
        .unwrap();
    assert_eq!(
        graph.read_binding(importer, "seen").unwrap().as_i32(),
        Some(2)
    );
    assert_eq!(
        graph.write_binding(importer, "seen", Value::from_i32(3)),
        Err(ModuleError::ImportedBinding)
    );
}

#[test]
/// Tarjan completion order and the published cycle root stay deterministic.
fn module_linking_completes_one_cycle_in_deterministic_stack_order() {
    let mut graph = test_graph();
    let a = graph
        .insert(record(
            "memory:a",
            &["memory:b"],
            vec![named_import("memory:b", "b", "seen_b")],
            vec![local_export("a", "a")],
            &["a"],
        ))
        .unwrap();
    let b = graph
        .insert(record(
            "memory:b",
            &["memory:c"],
            vec![named_import("memory:c", "c", "seen_c")],
            vec![local_export("b", "b")],
            &["b"],
        ))
        .unwrap();
    let c = graph
        .insert(record(
            "memory:c",
            &["memory:a"],
            vec![named_import("memory:a", "a", "seen_a")],
            vec![local_export("c", "c")],
            &["c"],
        ))
        .unwrap();

    let report = graph.link(a).unwrap();
    assert_eq!(report.components(), &[Box::from([c, b, a])]);
    for module in [a, b, c] {
        assert_eq!(
            graph.record(module).unwrap().status(),
            ModuleStatus::Linked { cycle_root: a }
        );
    }
    assert!(graph.link(a).unwrap().components().is_empty());
}

#[test]
/// A diamond graph follows source request order while linking its shared leaf once.
fn module_linking_preserves_requested_module_order_across_shared_dependencies() {
    let mut graph = test_graph();
    let root = graph
        .insert(record(
            "memory:root",
            &["memory:b", "memory:c"],
            vec![],
            vec![],
            &[],
        ))
        .unwrap();
    let b = graph
        .insert(record("memory:b", &["memory:d"], vec![], vec![], &[]))
        .unwrap();
    let c = graph
        .insert(record("memory:c", &["memory:d"], vec![], vec![], &[]))
        .unwrap();
    let d = graph
        .insert(record("memory:d", &[], vec![], vec![], &[]))
        .unwrap();

    let report = graph.link(root).unwrap();
    assert_eq!(
        report.components(),
        &[
            Box::from([d]),
            Box::from([b]),
            Box::from([c]),
            Box::from([root]),
        ]
    );
}

#[test]
/// Failure restores the active transaction without undoing a completed dependency SCC.
fn module_link_failure_rolls_back_only_incomplete_records() {
    let mut graph = test_graph();
    let dependency = graph
        .insert(record("memory:dependency", &[], vec![], vec![], &[]))
        .unwrap();
    let root = graph
        .insert(record(
            "memory:root",
            &["memory:dependency", "memory:missing"],
            vec![],
            vec![],
            &[],
        ))
        .unwrap();

    assert_eq!(graph.link(root), Err(ModuleError::MissingModule));
    assert!(matches!(
        graph.record(dependency).unwrap().status(),
        ModuleStatus::Linked { .. }
    ));
    assert_eq!(graph.record(root).unwrap().status(), ModuleStatus::Unlinked);
    graph
        .insert(record("memory:missing", &[], vec![], vec![], &[]))
        .unwrap();
    graph.link(root).unwrap();
    assert!(matches!(
        graph.record(root).unwrap().status(),
        ModuleStatus::Linked { .. }
    ));
}

#[test]
/// Named re-exports retain cell identity and reject a closed alias cycle.
fn module_indirect_exports_resolve_to_the_original_cell_and_detect_cycles() {
    let mut graph = test_graph();
    let source = graph
        .insert(record(
            "memory:source",
            &[],
            vec![],
            vec![local_export("x", "x")],
            &["x"],
        ))
        .unwrap();
    graph
        .insert(record(
            "memory:bridge",
            &["memory:source"],
            vec![],
            vec![indirect_export("y", "memory:source", "x")],
            &[],
        ))
        .unwrap();
    let consumer = graph
        .insert(record(
            "memory:consumer",
            &["memory:bridge"],
            vec![named_import("memory:bridge", "y", "seen")],
            vec![],
            &[],
        ))
        .unwrap();
    graph
        .write_binding(source, "x", Value::from_i32(9))
        .unwrap();
    graph.link(consumer).unwrap();
    assert_eq!(
        graph.read_binding(consumer, "seen").unwrap().as_i32(),
        Some(9)
    );

    let mut cycle = test_graph();
    let left = cycle
        .insert(record(
            "memory:left",
            &["memory:right"],
            vec![named_import("memory:right", "y", "seen")],
            vec![indirect_export("x", "memory:right", "y")],
            &[],
        ))
        .unwrap();
    cycle
        .insert(record(
            "memory:right",
            &["memory:left"],
            vec![],
            vec![indirect_export("y", "memory:left", "x")],
            &[],
        ))
        .unwrap();
    assert_eq!(cycle.link(left), Err(ModuleError::MissingExport));
    assert_eq!(cycle.record(left).unwrap().status(), ModuleStatus::Unlinked);
}

#[test]
/// Star cycles are ignored per branch, while a concrete sibling remains resolvable.
fn module_star_resolution_ignores_cycles_and_finds_concrete_siblings() {
    let mut graph = test_graph();
    let source = graph
        .insert(record(
            "memory:source",
            &[],
            vec![],
            vec![local_export("x", "x")],
            &["x"],
        ))
        .unwrap();
    graph
        .insert(record(
            "memory:right",
            &["memory:left"],
            vec![],
            vec![star_export("memory:left")],
            &[],
        ))
        .unwrap();
    graph
        .insert(record(
            "memory:left",
            &["memory:right", "memory:source"],
            vec![],
            vec![star_export("memory:right"), star_export("memory:source")],
            &[],
        ))
        .unwrap();
    let consumer = graph
        .insert(record(
            "memory:consumer",
            &["memory:left"],
            vec![named_import("memory:left", "x", "seen")],
            vec![],
            &[],
        ))
        .unwrap();
    graph
        .write_binding(source, "x", Value::from_i32(17))
        .unwrap();

    graph.link(consumer).unwrap();
    assert_eq!(
        graph.read_binding(consumer, "seen").unwrap().as_i32(),
        Some(17)
    );
}

#[test]
/// Diamond stars accept the same origin binding but reject two distinct origins.
fn module_star_resolution_compares_resolved_origin_identity() {
    let mut same = test_graph();
    let source = same
        .insert(record(
            "memory:source",
            &[],
            vec![],
            vec![local_export("x", "x")],
            &["x"],
        ))
        .unwrap();
    for side in ["memory:left", "memory:right"] {
        same.insert(record(
            side,
            &["memory:source"],
            vec![],
            vec![star_export("memory:source")],
            &[],
        ))
        .unwrap();
    }
    same.insert(record(
        "memory:bridge",
        &["memory:left", "memory:right"],
        vec![],
        vec![star_export("memory:left"), star_export("memory:right")],
        &[],
    ))
    .unwrap();
    let consumer = same
        .insert(record(
            "memory:consumer",
            &["memory:bridge"],
            vec![named_import("memory:bridge", "x", "seen")],
            vec![],
            &[],
        ))
        .unwrap();
    same.write_binding(source, "x", Value::from_i32(23))
        .unwrap();
    same.link(consumer).unwrap();
    assert_eq!(
        same.read_binding(consumer, "seen").unwrap().as_i32(),
        Some(23)
    );

    let mut ambiguous = test_graph();
    for side in ["memory:left", "memory:right"] {
        ambiguous
            .insert(record(
                side,
                &[],
                vec![],
                vec![local_export("x", "x")],
                &["x"],
            ))
            .unwrap();
    }
    ambiguous
        .insert(record(
            "memory:bridge",
            &["memory:left", "memory:right"],
            vec![],
            vec![star_export("memory:left"), star_export("memory:right")],
            &[],
        ))
        .unwrap();
    let consumer = ambiguous
        .insert(record(
            "memory:consumer",
            &["memory:bridge"],
            vec![named_import("memory:bridge", "x", "seen")],
            vec![],
            &[],
        ))
        .unwrap();
    assert_eq!(ambiguous.link(consumer), Err(ModuleError::AmbiguousExport));
}

#[test]
/// Star excludes default but treats the literal name `"*"` as an ordinary export name.
fn module_star_resolution_distinguishes_default_and_literal_star() {
    let mut graph = test_graph();
    graph
        .insert(record(
            "memory:source",
            &[],
            vec![],
            vec![
                local_export("default", "default"),
                local_export("*", "star"),
            ],
            &["default", "star"],
        ))
        .unwrap();
    graph
        .insert(record(
            "memory:bridge",
            &["memory:source"],
            vec![],
            vec![star_export("memory:source")],
            &[],
        ))
        .unwrap();
    let star_consumer = graph
        .insert(record(
            "memory:star-consumer",
            &["memory:bridge"],
            vec![named_import("memory:bridge", "*", "seen")],
            vec![],
            &[],
        ))
        .unwrap();
    graph.link(star_consumer).unwrap();

    let default_consumer = graph
        .insert(record(
            "memory:default-consumer",
            &["memory:bridge"],
            vec![named_import("memory:bridge", "default", "seen")],
            vec![],
            &[],
        ))
        .unwrap();
    assert_eq!(
        graph.link(default_consumer),
        Err(ModuleError::MissingExport)
    );
}

#[test]
/// Namespace is a binding variant and UTF-16 lone surrogates remain exact names.
fn module_namespace_and_utf16_export_names_remain_distinct() {
    let mut graph = test_graph();
    let lone = ModuleExportName::try_from_utf16(&[0xd800]).unwrap();
    graph
        .insert(record(
            "memory:source",
            &[],
            vec![],
            vec![local_export_name(lone.clone(), "lone")],
            &["lone"],
        ))
        .unwrap();
    graph
        .insert(record(
            "memory:bridge",
            &["memory:source"],
            vec![],
            vec![
                namespace_export("ns", "memory:source"),
                star_export("memory:source"),
            ],
            &[],
        ))
        .unwrap();
    let consumer = graph
        .insert(record(
            "memory:consumer",
            &["memory:bridge", "memory:source"],
            vec![
                named_import("memory:bridge", "ns", "namespace"),
                named_import_name("memory:bridge", lone, "lone"),
                namespace_import("memory:source", "direct_namespace"),
            ],
            vec![],
            &[],
        ))
        .unwrap();

    graph.link(consumer).unwrap();
    assert_eq!(
        graph.read_binding(consumer, "namespace"),
        Err(ModuleError::NamespaceObjectRequired)
    );
    assert!(
        graph
            .record(consumer)
            .unwrap()
            .imports()
            .iter()
            .all(|entry| entry.resolved().is_some())
    );
}

#[test]
/// Record publication rejects duplicate identity and every configured hard limit.
fn module_graph_rejects_duplicate_records_and_checked_limit_overflow() {
    let mut graph = ModuleGraph::try_new(ModuleLimits::new(1, 1, 1)).unwrap();
    graph
        .insert(record(
            "memory:a",
            &[],
            vec![],
            vec![local_export("x", "x")],
            &["x"],
        ))
        .unwrap();
    assert_eq!(
        graph.insert(record("memory:a", &[], vec![], vec![], &[])),
        Err(ModuleError::DuplicateSpecifier)
    );
    assert_eq!(
        graph.insert(record("memory:b", &[], vec![], vec![], &[])),
        Err(ModuleError::ModuleLimit { limit: 1 })
    );

    let mut edges = ModuleGraph::try_new(ModuleLimits::new(2, 2, 0)).unwrap();
    assert_eq!(
        edges.insert(record("memory:root", &["memory:dep"], vec![], vec![], &[])),
        Err(ModuleError::EdgeLimit { limit: 0 })
    );
}

#[test]
/// A deep linear graph proves linking does not consume the native Rust call stack.
fn module_linking_uses_iterative_worklists_for_deep_graphs() {
    const MODULES: usize = 1_000;
    let mut graph =
        ModuleGraph::try_new(ModuleLimits::new(MODULES as u32, 0, (MODULES - 1) as u32)).unwrap();
    let mut root = None;
    for index in 0..MODULES {
        let current = format!("memory:{index}");
        let requested_modules = if index + 1 == MODULES {
            Box::new([])
        } else {
            let next = format!("memory:{}", index + 1);
            vec![specifier(&next)].into_boxed_slice()
        };
        let id = graph
            .insert(ModuleRecordInit {
                specifier: specifier(&current),
                requested_modules,
                imports: Box::new([]),
                exports: Box::new([]),
                local_bindings: Box::new([]),
                has_top_level_await: false,
            })
            .unwrap();
        root.get_or_insert(id);
    }
    let report = graph.link(root.unwrap()).unwrap();
    assert_eq!(report.components().len(), MODULES);
    assert!(
        report
            .components()
            .iter()
            .all(|component| component.len() == 1)
    );
}

struct MemoryLoader {
    modules: Vec<(ModuleIdentity, Option<LoadedModule>)>,
}

impl MemoryLoader {
    fn new(modules: Vec<LoadedModule>) -> Self {
        Self {
            modules: modules
                .into_iter()
                .map(|module| (module.identity().clone(), Some(module)))
                .collect(),
        }
    }
}

impl ModuleLoader for MemoryLoader {
    type Error = ();

    fn resolve(
        &mut self,
        request: &tachyon_bytecode::ModuleRequest,
        _referrer: Option<&ModuleIdentity>,
    ) -> Result<ModuleIdentity, Self::Error> {
        let text = String::from_utf16(request.specifier.as_ref()).map_err(|_| ())?;
        Ok(specifier(&text))
    }

    fn load(
        &mut self,
        resolved: ResolvedModuleRequest<'_>,
    ) -> Result<Option<LoadedModule>, Self::Error> {
        Ok(self
            .modules
            .iter_mut()
            .find(|(specifier, _)| specifier == resolved.identity())
            .and_then(|(_, module)| module.take()))
    }
}

struct PrecompiledLoader {
    identity: ModuleIdentity,
    module: Option<LoadedModule>,
}

impl ModuleLoader for PrecompiledLoader {
    type Error = ();

    fn resolve(
        &mut self,
        request: &tachyon_bytecode::ModuleRequest,
        _referrer: Option<&ModuleIdentity>,
    ) -> Result<ModuleIdentity, Self::Error> {
        let text = String::from_utf16(request.specifier.as_ref()).map_err(|_| ())?;
        Ok(specifier(&text))
    }

    fn load(
        &mut self,
        resolved: ResolvedModuleRequest<'_>,
    ) -> Result<Option<LoadedModule>, Self::Error> {
        if resolved.identity() == &self.identity {
            Ok(self.module.take())
        } else {
            Ok(None)
        }
    }
}

fn loaded(record: ModuleRecordInit, body: ModuleBody) -> LoadedModule {
    LoadedModule::new(record, body)
}

/// Exercises one loader/link/evaluate path under every supported dispatch batch.
fn assert_memory_pipeline_batch<const N: usize>(forced_major: bool) {
    let mut isolate = fixtures::test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let dependency = loaded(
        record("memory:dependency", &[], vec![], vec![], &[]),
        ModuleBody::Synthetic,
    );
    let root = loaded(
        record("memory:root", &["memory:dependency"], vec![], vec![], &[]),
        ModuleBody::Precompiled(fixtures::arithmetic_module()),
    );
    let mut loader = MemoryLoader::new(vec![root, dependency]);
    let root = isolate
        .load_module_graph(&mut loader, &specifier("memory:root"))
        .unwrap();
    assert_eq!(
        isolate.evaluate_module_with_test_batch::<N>(root).unwrap(),
        RunOutcome::Completed(Value::from_i32(3))
    );
    assert_eq!(
        isolate.evaluate_module(root).unwrap(),
        RunOutcome::Completed(Value::from_i32(3))
    );
}

#[test]
fn isolate_owned_module_pipeline_loads_links_and_evaluates_once() {
    assert_memory_pipeline_batch::<1>(false);
    assert_memory_pipeline_batch::<2>(false);
    assert_memory_pipeline_batch::<4>(false);
    assert_memory_pipeline_batch::<8>(true);
    assert_memory_pipeline_batch::<16>(true);
}

#[test]
fn module_loader_uses_the_resolved_identity_as_the_publication_key() {
    let mut isolate = fixtures::test_isolate();
    let mut loader = MemoryLoader::new(vec![loaded(
        record("memory:root", &[], vec![], vec![], &[]),
        ModuleBody::Synthetic,
    )]);
    let root = isolate
        .load_module_graph(&mut loader, &specifier("memory:root"))
        .expect("resolved identity is authoritative");
    assert_eq!(
        isolate.evaluate_module(root).unwrap(),
        RunOutcome::Completed(Value::from_immediate(Immediate::Undefined))
    );
}

#[test]
fn failed_module_load_rolls_back_every_record_from_the_transaction() {
    let mut isolate = fixtures::test_isolate();
    let root_record = || record("memory:root", &["memory:dependency"], vec![], vec![], &[]);
    let mut incomplete = MemoryLoader::new(vec![loaded(root_record(), ModuleBody::Synthetic)]);
    assert!(matches!(
        isolate.load_module_graph(&mut incomplete, &specifier("memory:root")),
        Err(ModuleLoadError::Missing(_))
    ));

    let mut complete = MemoryLoader::new(vec![
        loaded(root_record(), ModuleBody::Synthetic),
        loaded(
            record("memory:dependency", &[], vec![], vec![], &[]),
            ModuleBody::Synthetic,
        ),
    ]);
    let root = isolate
        .load_module_graph(&mut complete, &specifier("memory:root"))
        .unwrap();
    assert_eq!(
        isolate.evaluate_module(root).unwrap(),
        RunOutcome::Completed(Value::from_immediate(Immediate::Undefined))
    );
}

#[test]
fn top_level_await_stops_at_the_explicit_async_evaluation_boundary() {
    let mut isolate = fixtures::test_isolate();
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(1),
                SourceName::new("memory:async"),
                MediaType::Mjs,
                "await 1;".into(),
            ),
            CompileOptions {
                source_mode: SourceMode::Module,
                ..CompileOptions::default()
            },
        )
        .expect("top-level await module should compile");
    let mut loader = PrecompiledLoader {
        identity: specifier("memory:async"),
        module: Some(LoadedModule::precompiled(module)),
    };
    let root = isolate
        .load_module_graph(&mut loader, &specifier("memory:async"))
        .unwrap();
    assert_eq!(
        isolate.evaluate_module(root),
        Err(ModuleEvaluationError::AsyncEvaluationRequired(root))
    );
}

#[test]
/// Materializes one namespace identity while preserving live cells and exotic mutations.
fn module_namespace_materializes_live_exports_and_exotic_descriptors() {
    let mut isolate = fixtures::test_isolate();
    let exporter = isolate
        .module_graph
        .insert(record(
            "memory:exporter",
            &[],
            vec![],
            vec![local_export("value", "value")],
            &["value"],
        ))
        .unwrap();
    let consumer = isolate
        .module_graph
        .insert(record(
            "memory:consumer",
            &["memory:exporter"],
            vec![namespace_import("memory:exporter", "namespace")],
            vec![],
            &[],
        ))
        .unwrap();
    isolate.module_graph.link(consumer).unwrap();
    isolate
        .write_module_binding(exporter, "value", Value::from_i32(1))
        .unwrap();

    let namespace = isolate.read_module_binding(consumer, "namespace").unwrap();
    assert_eq!(
        isolate.read_module_binding(consumer, "namespace").unwrap(),
        namespace
    );
    let value = isolate.intern_intrinsic_name(b"value").unwrap();
    assert_eq!(
        isolate.get_data_property(namespace, value).unwrap(),
        Some(Value::from_i32(1))
    );
    isolate
        .write_module_binding(exporter, "value", Value::from_i32(2))
        .unwrap();
    assert_eq!(
        isolate.get_data_property(namespace, value).unwrap(),
        Some(Value::from_i32(2))
    );

    let descriptor = isolate
        .complete_own_property_descriptor(namespace, value)
        .unwrap()
        .unwrap();
    assert!(matches!(
        descriptor,
        PropertyDescriptor::Data(DataPropertyDescriptor {
            value: Some(current),
            writable: Some(true),
            enumerable: Some(true),
            configurable: Some(false),
        }) if current == Value::from_i32(2)
    ));
    assert!(matches!(
        isolate.resolve_property_write(namespace, value.into(), Value::from_i32(3)),
        Ok(PropertyWrite::Complete(false))
    ));
    assert!(!isolate.delete_own_data_property(namespace, value).unwrap());
    isolate
        .define_data_property(
            namespace,
            value,
            DataPropertyDescriptor {
                value: Some(Value::from_i32(2)),
                ..DataPropertyDescriptor::default()
            },
        )
        .unwrap();
    assert!(matches!(
        isolate.define_data_property(
            namespace,
            value,
            DataPropertyDescriptor {
                value: Some(Value::from_i32(3)),
                ..DataPropertyDescriptor::default()
            },
        ),
        Err(ExecutionError::InvalidPropertyRedefinition(target)) if target == namespace
    ));
    let (_, ordinary) = isolate.object_snapshot(namespace).unwrap();
    assert_eq!(ordinary.prototype.as_immediate(), Some(Immediate::Null));
    assert!(!ordinary.extensible);
    assert!(
        isolate
            .ordinary_set_prototype_of(namespace, Value::from_immediate(Immediate::Null))
            .unwrap()
    );
    let replacement_prototype = isolate.create_ordinary_object().unwrap();
    assert!(
        !isolate
            .ordinary_set_prototype_of(namespace, replacement_prototype)
            .unwrap()
    );
    let tag = isolate.module_namespace_to_string_tag_key().unwrap();
    assert!(matches!(
        isolate
            .complete_own_property_descriptor(namespace, tag)
            .unwrap(),
        Some(PropertyDescriptor::Data(DataPropertyDescriptor {
            writable: Some(false),
            enumerable: Some(false),
            configurable: Some(true),
            ..
        }))
    ));
}

#[test]
/// Filters ambiguous stars and preserves UTF-16 lexical ordering in namespace own keys.
fn module_namespace_own_keys_follow_exported_names_and_utf16_order() {
    let mut isolate = fixtures::test_isolate();
    let left = isolate
        .module_graph
        .insert(record(
            "memory:left",
            &[],
            vec![],
            vec![local_export("shared", "left")],
            &["left"],
        ))
        .unwrap();
    let right = isolate
        .module_graph
        .insert(record(
            "memory:right",
            &[],
            vec![],
            vec![local_export("shared", "right")],
            &["right"],
        ))
        .unwrap();
    let root = isolate
        .module_graph
        .insert(record(
            "memory:root",
            &["memory:left", "memory:right"],
            vec![],
            vec![
                local_export("default", "default_value"),
                local_export("z", "z"),
                local_export_name(
                    ModuleExportName::try_from_utf16(&[0xd800]).unwrap(),
                    "surrogate",
                ),
                star_export("memory:left"),
                star_export("memory:right"),
            ],
            &["default_value", "z", "surrogate"],
        ))
        .unwrap();
    isolate.module_graph.link(root).unwrap();
    isolate
        .write_module_binding(left, "left", Value::from_i32(1))
        .unwrap();
    isolate
        .write_module_binding(right, "right", Value::from_i32(2))
        .unwrap();
    for (name, value) in [
        ("default_value", Value::from_i32(3)),
        ("z", Value::from_i32(4)),
        ("surrogate", Value::from_i32(5)),
    ] {
        isolate.write_module_binding(root, name, value).unwrap();
    }

    let namespace = isolate.get_module_namespace(root).unwrap();
    let (_, snapshot) = isolate.object_snapshot(namespace).unwrap();
    let keys = isolate
        .ordinary_own_property_keys(namespace, snapshot)
        .unwrap()
        .collect::<Vec<_>>();
    let default = isolate.intern_intrinsic_name(b"default").unwrap();
    let z = isolate.intern_intrinsic_name(b"z").unwrap();
    let surrogate = isolate
        .atoms
        .try_intern(JsString::try_from_utf16(&[0xd800]).unwrap())
        .unwrap();
    let tag = isolate.module_namespace_to_string_tag_key().unwrap();
    assert_eq!(keys, vec![default.into(), z.into(), surrogate.into(), tag]);
    let shared = isolate.intern_intrinsic_name(b"shared").unwrap();
    assert_eq!(isolate.get_data_property(namespace, shared).unwrap(), None);
    assert_eq!(
        isolate.get_data_property(namespace, surrogate).unwrap(),
        Some(Value::from_i32(5))
    );
}

#[test]
/// Keeps the cached namespace and its live exported object rooted through forced major GC.
fn module_namespace_cache_and_live_values_survive_forced_major_collection() {
    let mut isolate = fixtures::test_isolate();
    let root = isolate
        .module_graph
        .insert(record(
            "memory:root",
            &[],
            vec![],
            vec![local_export("object", "object")],
            &["object"],
        ))
        .unwrap();
    isolate.module_graph.link(root).unwrap();
    let object = isolate.create_ordinary_object().unwrap();
    isolate
        .write_module_binding(root, "object", object)
        .unwrap();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let namespace = isolate.get_module_namespace(root).unwrap();
    let object_key = isolate.intern_intrinsic_name(b"object").unwrap();
    for _ in 0..8 {
        isolate.create_ordinary_object().unwrap();
    }
    let cached = isolate.get_module_namespace(root).unwrap();
    assert_eq!(cached, namespace);
    let retained = isolate
        .get_data_property(cached, object_key)
        .unwrap()
        .unwrap();
    let raw = retained.as_heap_ref().unwrap();
    assert!(
        isolate
            .heap
            .checked_reference(raw, isolate.types.ordinary_object)
            .is_ok()
    );
}

#[test]
fn module_namespace_property_read_runs_for_every_dispatch_batch() {
    assert_module_namespace_dispatch_batch::<1>(2_301);
    assert_module_namespace_dispatch_batch::<2>(2_302);
    assert_module_namespace_dispatch_batch::<4>(2_304);
    assert_module_namespace_dispatch_batch::<8>(2_308);
    assert_module_namespace_dispatch_batch::<16>(2_316);
}

#[test]
/// Exercises the tuned atom-index path used once namespace exports exceed the linear limit.
fn module_namespace_large_export_table_uses_indexed_live_lookup() {
    const NAMES: [&str; 10] = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"];
    let mut isolate = fixtures::test_isolate();
    let root = isolate
        .module_graph
        .insert(record(
            "memory:root",
            &[],
            vec![],
            NAMES.iter().map(|name| local_export(name, name)).collect(),
            &NAMES,
        ))
        .unwrap();
    isolate.module_graph.link(root).unwrap();
    isolate
        .write_module_binding(root, "j", Value::from_i32(10))
        .unwrap();
    let namespace = isolate.get_module_namespace(root).unwrap();
    let key = isolate.intern_intrinsic_name(b"j").unwrap();
    assert_eq!(
        isolate.get_data_property(namespace, key).unwrap(),
        Some(Value::from_i32(10))
    );
}

/// Publishes a namespace into the global object and executes a real property-read opcode.
fn assert_module_namespace_dispatch_batch<const N: usize>(source_id: u32) {
    let mut isolate = fixtures::test_isolate();
    let root = isolate
        .module_graph
        .insert(record(
            "memory:root",
            &[],
            vec![],
            vec![local_export("value", "value")],
            &["value"],
        ))
        .unwrap();
    isolate.module_graph.link(root).unwrap();
    isolate
        .write_module_binding(root, "value", Value::from_i32(7))
        .unwrap();
    let namespace = isolate.get_module_namespace(root).unwrap();
    let namespace_atom = isolate.intern_intrinsic_name(b"namespace").unwrap();
    isolate.realm.set(namespace_atom, namespace).unwrap();
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("module-namespace-dispatch"),
                MediaType::JavaScript,
                "namespace.value === 7;".into(),
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 128,
                quantum: 128,
            },
        )
        .unwrap();
    assert!(
        matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ),
        "dispatch batch {N} returned {outcome:?}"
    );
}

#[test]
fn module_live_cells_are_roots_at_allocation_triggered_major_collections() {
    let mut isolate = fixtures::test_isolate();
    let mut loader = MemoryLoader::new(vec![loaded(
        record(
            "memory:root",
            &[],
            vec![],
            vec![local_export("value", "value")],
            &["value"],
        ),
        ModuleBody::Synthetic,
    )]);
    let root = isolate
        .load_module_graph(&mut loader, &specifier("memory:root"))
        .unwrap();
    let object = isolate.create_ordinary_object().unwrap();
    isolate.write_module_binding(root, "value", object).unwrap();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    for _ in 0..32 {
        isolate.create_ordinary_object().unwrap();
    }
    let retained = isolate.read_module_binding(root, "value").unwrap();
    let raw = retained
        .as_heap_ref()
        .expect("module binding retains object");
    assert!(
        isolate
            .heap
            .checked_reference(raw, isolate.types.ordinary_object)
            .is_ok()
    );
}
