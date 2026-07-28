use super::*;
use crate::module::*;

fn specifier(value: &str) -> ModuleSpecifier {
    ModuleSpecifier::try_new(value).expect("test module specifier allocates")
}

fn binding(value: &str) -> BindingName {
    BindingName::try_new(value).expect("test module binding allocates")
}

fn request_list(values: &[&str]) -> Box<[ModuleSpecifier]> {
    values
        .iter()
        .map(|value| specifier(value))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn binding_list(values: &[&str]) -> Box<[BindingName]> {
    values
        .iter()
        .map(|value| binding(value))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn local_export(export_name: &str, local_name: &str) -> ExportEntry {
    ExportEntry::Local {
        export_name: binding(export_name),
        local_name: binding(local_name),
    }
}

fn indirect_export(export_name: &str, module_request: &str, import_name: &str) -> ExportEntry {
    ExportEntry::Indirect {
        export_name: binding(export_name),
        module_request: specifier(module_request),
        import_name: binding(import_name),
    }
}

fn named_import(module_request: &str, import_name: &str, local_name: &str) -> ImportEntry {
    ImportEntry::new(
        specifier(module_request),
        binding(import_name),
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
            .resolved_cell()
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
    assert_eq!(cycle.link(left), Err(ModuleError::CircularExport));
    assert_eq!(cycle.record(left).unwrap().status(), ModuleStatus::Unlinked);
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
    modules: Vec<(ModuleSpecifier, Option<LoadedModule>)>,
}

impl MemoryLoader {
    fn new(modules: Vec<LoadedModule>) -> Self {
        Self {
            modules: modules
                .into_iter()
                .map(|module| (module.record.specifier.clone(), Some(module)))
                .collect(),
        }
    }
}

impl ModuleLoader for MemoryLoader {
    type Error = ();

    fn resolve(
        &mut self,
        request: &ModuleSpecifier,
        _referrer: Option<&ModuleSpecifier>,
    ) -> Result<ModuleSpecifier, Self::Error> {
        Ok(request.clone())
    }

    fn load(&mut self, resolved: &ModuleSpecifier) -> Result<Option<LoadedModule>, Self::Error> {
        Ok(self
            .modules
            .iter_mut()
            .find(|(specifier, _)| specifier == resolved)
            .and_then(|(_, module)| module.take()))
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
fn module_loader_rejects_identity_substitution_before_publication() {
    let mut isolate = fixtures::test_isolate();
    let mut loader = MemoryLoader::new(vec![loaded(
        record("memory:other", &[], vec![], vec![], &[]),
        ModuleBody::Synthetic,
    )]);
    loader.modules[0].0 = specifier("memory:root");
    assert!(matches!(
        isolate.load_module_graph(&mut loader, &specifier("memory:root")),
        Err(ModuleLoadError::Graph(ModuleError::LoaderIdentityMismatch))
    ));
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
    let mut loader = MemoryLoader::new(vec![loaded(
        record("memory:async", &[], vec![], vec![], &[]),
        ModuleBody::AsyncPrecompiled(fixtures::arithmetic_module()),
    )]);
    let root = isolate
        .load_module_graph(&mut loader, &specifier("memory:async"))
        .unwrap();
    assert_eq!(
        isolate.evaluate_module(root),
        Err(ModuleEvaluationError::AsyncEvaluationRequired(root))
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
