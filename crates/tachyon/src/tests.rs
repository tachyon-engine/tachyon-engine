use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};
use tachyon_gc::{HeapLimit, SPAN_SIZE_BYTES};
use tachyon_vm::{
    AtomHashSeed, AtomTableConfig, ExecutionBudget, ExecutionError, Isolate, IsolateConfig,
    RealmLimits, RunOutcome, StackLimits,
};

fn test_isolate() -> Isolate {
    test_isolate_with_realm_limits(RealmLimits::new(64, 1_024).with_max_shapes(1_024))
}

fn test_isolate_with_realm_limits(realm_limits: RealmLimits) -> Isolate {
    Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(8 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        realm_limits,
    ))
    .expect("test isolate descriptors register")
}

/// Compiles and executes one in-memory script with enough budget for expression fixtures.
fn execute_source(source_id: u32, text: &str) -> tachyon_value::Value {
    execute_source_with_heap(source_id, text, HeapLimit::new(8 * SPAN_SIZE_BYTES))
}

/// Compiles and executes one fixture with an explicit heap budget for payload-growth regressions.
fn execute_source_with_heap(
    source_id: u32,
    text: &str,
    heap_limit: HeapLimit,
) -> tachyon_value::Value {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("embedded-input"),
                MediaType::JavaScript,
                Arc::from(text),
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        heap_limit,
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024).with_max_shapes(1_024),
    ))
    .expect("test isolate descriptors register");
    match isolate
        .execute(
            &module,
            ExecutionBudget {
                fuel: 256,
                quantum: 256,
            },
        )
        .unwrap()
    {
        RunOutcome::Completed(value) => value,
        outcome => panic!("expression fixture did not complete: {outcome:?}; source: {text}"),
    }
}

mod builtins;
mod classes;
mod control_flow;
mod execution;
mod expressions;
mod functions;
mod promise;
