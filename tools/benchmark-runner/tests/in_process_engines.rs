use std::sync::Arc;

use benchmark_runner::{
    AdapterError, BenchmarkAdapter, BenchmarkRequest, BoaInProcessAdapter, EngineIdentity,
    EngineKind, MeasurementMode, RQuickJsInProcessAdapter, ScriptEntry,
};

fn identity(name: &str, kind: EngineKind, version: &str) -> EngineIdentity {
    EngineIdentity {
        name: name.into(),
        kind,
        version: version.into(),
        commit: format!("crates.io:{name}@{version}").into(),
        features: "default".into(),
        build_flags: "test".into(),
        binary_size_bytes: None,
    }
}

fn make_request(mode: MeasurementMode) -> BenchmarkRequest {
    BenchmarkRequest {
        script_id: "fixture/main".into(),
        entry: ScriptEntry::MainFunction,
        source: Arc::from("function main() { return 1 + 1; }"),
        mode,
        iterations: 2,
    }
}

#[test]
fn boa_executes_exact_steady_state_work_and_rejects_other_boundaries() {
    let mut adapter =
        BoaInProcessAdapter::new(identity("boa_engine", EngineKind::BoaInProcess, "0.21.0"))
            .unwrap();
    let request = make_request(MeasurementMode::SteadyState);
    adapter.prepare(&request).unwrap();
    assert_eq!(adapter.sample(&request).unwrap().iterations, 2);
    assert_eq!(
        adapter.prepare(&make_request(MeasurementMode::ColdStart)),
        Err(AdapterError::UnsupportedMode(MeasurementMode::ColdStart))
    );
}

#[test]
fn rquickjs_executes_exact_steady_state_work_and_rejects_other_boundaries() {
    let mut adapter = RQuickJsInProcessAdapter::new(identity(
        "rquickjs",
        EngineKind::RQuickJsInProcess,
        "0.12.1",
    ))
    .unwrap();
    let request = make_request(MeasurementMode::SteadyState);
    adapter.prepare(&request).unwrap();
    assert_eq!(adapter.sample(&request).unwrap().iterations, 2);
    assert_eq!(
        adapter.prepare(&make_request(MeasurementMode::PrecompiledExecute)),
        Err(AdapterError::UnsupportedMode(
            MeasurementMode::PrecompiledExecute
        ))
    );
}
