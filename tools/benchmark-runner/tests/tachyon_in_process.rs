use std::sync::Arc;

use benchmark_runner::{
    AdapterError, BenchmarkAdapter, BenchmarkConfig, BenchmarkRequest, CorpusScript,
    EngineIdentity, EngineKind, MeasurementMode, TachyonInProcessAdapter, TachyonInProcessConfig,
    load_corpus,
};

const CONFIG: &str = include_str!("../../../benchmark_config.toml");

fn config_and_corpus() -> (BenchmarkConfig, Vec<CorpusScript>) {
    let mut config = BenchmarkConfig::parse(CONFIG).unwrap();
    config.tachyon.steady_state_iterations = 8;
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
        source: Arc::clone(&script.source),
        mode,
    }
}

/// Proves all three timing boundaries execute the checked-in Oxc-to-VM foundation slice.
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
        MeasurementMode::SteadyState,
    ] {
        let request = request(script, mode);
        adapter.prepare(&request).unwrap();
        let metrics = adapter.sample(&request).unwrap();
        assert!(metrics.elapsed_ns > 0);
        assert_eq!(
            metrics.iterations,
            if mode == MeasurementMode::SteadyState {
                8
            } else {
                1
            }
        );
    }
}

/// Keeps process startup and unsupported future syntax out of successful in-process measurements.
#[test]
fn tachyon_adapter_rejects_process_cold_start_and_unsupported_corpus() {
    let (config, corpus) = config_and_corpus();
    let tachyon_script = corpus
        .iter()
        .find(|script| script.config.id.as_ref() == "tachyon/foundation-arithmetic")
        .unwrap();
    let boa_script = corpus
        .iter()
        .find(|script| script.config.id.as_ref() == "basic/closure")
        .unwrap();
    let mut adapter = adapter(&config);

    let cold = request(tachyon_script, MeasurementMode::ColdStart);
    assert_eq!(
        adapter.prepare(&cold),
        Err(AdapterError::UnsupportedMode(MeasurementMode::ColdStart))
    );

    let unsupported = request(boa_script, MeasurementMode::PrecompiledExecute);
    assert!(matches!(
        adapter.prepare(&unsupported),
        Err(AdapterError::Setup(message)) if message.contains("compile failed")
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
    let request = request(script, MeasurementMode::ParseCompileExecute);

    adapter.prepare(&request).unwrap();
    assert!(matches!(
        adapter.sample(&request),
        Err(AdapterError::Engine(message)) if message.contains("compile failed")
    ));
}
