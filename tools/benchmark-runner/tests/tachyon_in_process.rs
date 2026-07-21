use std::sync::Arc;

use benchmark_runner::{
    AdapterError, BenchmarkAdapter, BenchmarkConfig, BenchmarkRequest, CorpusScript,
    EngineIdentity, EngineKind, MeasurementMode, TachyonInProcessAdapter, TachyonInProcessConfig,
    load_corpus,
};

const CONFIG: &str = include_str!("../../../benchmark_config.toml");

fn config_and_corpus() -> (BenchmarkConfig, Vec<CorpusScript>) {
    let config = BenchmarkConfig::parse(CONFIG).unwrap();
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let corpus = load_corpus(&workspace, &config).unwrap();
    (config, corpus)
}

fn adapter(config: &BenchmarkConfig) -> TachyonInProcessAdapter {
    TachyonInProcessAdapter::new(
        EngineIdentity {
            name: "Tachyon".into(),
            kind: EngineKind::TachyonInProcess,
            version: "fixture".into(),
            commit: "fixture".into(),
            features: "default".into(),
            build_flags: "fixture".into(),
            binary_size_bytes: None,
        },
        TachyonInProcessConfig::from_benchmark(config.tachyon),
    )
    .unwrap()
}

fn request(script: &CorpusScript, mode: MeasurementMode) -> BenchmarkRequest {
    BenchmarkRequest {
        script_id: script.config.id.clone(),
        entry: script.config.entry,
        source: Arc::clone(&script.source),
        mode,
        iterations: if mode == MeasurementMode::SteadyState {
            script.config.steady_state_iterations
        } else {
            1
        },
    }
}

/// Proves script and main-function entries use only timing boundaries they can repeat honestly.
#[test]
fn tachyon_adapter_executes_all_honest_in_process_modes() {
    let (config, corpus) = config_and_corpus();
    let script = corpus
        .iter()
        .find(|script| script.config.id.as_ref() == "tachyon/foundation-arithmetic")
        .unwrap();
    let mut adapter = adapter(&config);

    for mode in [
        MeasurementMode::ParseCompileExecute,
        MeasurementMode::PrecompiledExecute,
    ] {
        let request = request(script, mode);
        adapter.prepare(&request).unwrap();
        let metrics = adapter.sample(&request).unwrap();
        assert!(metrics.elapsed_ns > 0);
        assert_eq!(metrics.iterations, 1);
    }
    let main_script = corpus
        .iter()
        .find(|script| script.config.id.as_ref() == "basic/call-loop")
        .unwrap();
    let request = request(main_script, MeasurementMode::SteadyState);
    adapter.prepare(&request).unwrap();
    let metrics = adapter.sample(&request).unwrap();
    assert!(metrics.elapsed_ns > 0);
    assert_eq!(metrics.iterations, 1);
}

/// Keeps process startup and malformed source out of successful in-process measurements.
#[test]
fn tachyon_adapter_rejects_process_cold_start_and_unsupported_syntax() {
    let (config, corpus) = config_and_corpus();
    let tachyon_script = corpus
        .iter()
        .find(|script| script.config.id.as_ref() == "tachyon/foundation-arithmetic")
        .unwrap();
    let script = corpus
        .iter()
        .find(|script| script.config.id.as_ref() == "basic/closure")
        .unwrap();
    let mut adapter = adapter(&config);

    let cold = request(tachyon_script, MeasurementMode::ColdStart);
    assert_eq!(
        adapter.prepare(&cold),
        Err(AdapterError::UnsupportedMode(MeasurementMode::ColdStart))
    );

    let repeated_script = request(tachyon_script, MeasurementMode::SteadyState);
    assert!(matches!(
        adapter.prepare(&repeated_script),
        Err(AdapterError::Setup(message)) if message.contains("main-function")
    ));

    let mut unsupported = request(script, MeasurementMode::PrecompiledExecute);
    unsupported.source = Arc::from("function {");
    assert!(matches!(
        adapter.prepare(&unsupported),
        Err(AdapterError::Setup(message)) if message.contains("compile failed")
    ));

    let mut repeated_compile = request(tachyon_script, MeasurementMode::PrecompiledExecute);
    repeated_compile.iterations = 2;
    assert!(matches!(
        adapter.prepare(&repeated_compile),
        Err(AdapterError::Setup(message)) if message.contains("iteration count")
    ));
}

/// Ensures parse/compile errors occur during the timed sample rather than untimed preparation.
#[test]
fn parse_compile_mode_keeps_compile_failures_inside_the_sample_boundary() {
    let (config, corpus) = config_and_corpus();
    let script = corpus
        .iter()
        .find(|script| script.config.id.as_ref() == "basic/closure")
        .unwrap();
    let mut adapter = adapter(&config);
    let mut request = request(script, MeasurementMode::ParseCompileExecute);
    request.source = Arc::from("function {");

    adapter.prepare(&request).unwrap();
    assert!(matches!(
        adapter.sample(&request),
        Err(AdapterError::Engine(message)) if message.contains("compile failed")
    ));
}

#[test]
fn main_function_corpus_executes_through_separate_invocation_code() {
    let (config, corpus) = config_and_corpus();
    let script = corpus
        .iter()
        .find(|script| script.config.id.as_ref() == "basic/call-loop")
        .unwrap();
    let mut adapter = adapter(&config);
    for mode in [
        MeasurementMode::PrecompiledExecute,
        MeasurementMode::SteadyState,
    ] {
        let request = request(script, mode);
        adapter.prepare(&request).unwrap();
        assert!(adapter.sample(&request).unwrap().elapsed_ns > 0);
    }
}

#[test]
fn main_function_setup_can_publish_global_lexical_closure_state() {
    let (config, corpus) = config_and_corpus();
    let script = corpus
        .iter()
        .find(|script| script.config.id.as_ref() == "basic/closure")
        .unwrap();
    let mut request = request(script, MeasurementMode::SteadyState);
    request.source = Arc::from(
        "function outer() { let value = 42; return function() { return value; }; } let invoke = outer(); function main() { return invoke(); }",
    );
    let mut adapter = adapter(&config);
    adapter.prepare(&request).unwrap();
    let metrics = adapter.sample(&request).unwrap();
    assert!(metrics.elapsed_ns > 0);
    assert_eq!(metrics.iterations, 1);
}

#[test]
/// Proves the adapter executes exactly the requested count and rejects post-prepare drift.
fn steady_state_uses_and_freezes_the_request_work_count() {
    let (config, corpus) = config_and_corpus();
    let script = corpus
        .iter()
        .find(|script| script.config.id.as_ref() == "basic/call-loop")
        .unwrap();
    let mut request = request(script, MeasurementMode::SteadyState);
    request.source = Arc::from("function main() { return 42; }");
    request.iterations = 3;
    let mut adapter = adapter(&config);
    adapter.prepare(&request).unwrap();
    assert_eq!(adapter.sample(&request).unwrap().iterations, 3);

    request.iterations = 4;
    assert!(matches!(
        adapter.sample(&request),
        Err(AdapterError::Setup(message)) if message.contains("differs from prepared")
    ));
}
