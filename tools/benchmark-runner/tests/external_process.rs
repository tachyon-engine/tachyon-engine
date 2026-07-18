#![cfg(unix)]

use std::{ffi::OsString, path::PathBuf, sync::Arc, time::Duration};

use benchmark_runner::{
    AdapterError, BenchmarkAdapter, BenchmarkRequest, EngineIdentity, EngineKind,
    ExternalProcessAdapter, ExternalProcessConfig, MeasurementMode, ScriptEntry,
};

fn adapter(timeout: Duration, output_limit: usize) -> ExternalProcessAdapter {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-engine.sh");
    ExternalProcessAdapter::new(
        EngineIdentity {
            name: "fixture CLI".into(),
            kind: EngineKind::EscargotCli,
            version: "fixture".into(),
            commit: "fixture".into(),
            features: "none".into(),
            build_flags: "fixture".into(),
            binary_size_bytes: None,
        },
        ExternalProcessConfig {
            executable: "/bin/sh".into(),
            fixed_arguments: vec![OsString::from(fixture)],
            timeout,
            maximum_output_bytes: output_limit,
        },
    )
    .unwrap()
}

fn request(source: &str, mode: MeasurementMode) -> BenchmarkRequest {
    BenchmarkRequest {
        script_id: "fixture".into(),
        entry: ScriptEntry::Script,
        source: Arc::from(source),
        mode,
        iterations: 1,
    }
}

#[test]
fn external_adapter_executes_prepared_source_and_records_binary_size() {
    let mut adapter = adapter(Duration::from_secs(1), 1024);
    let request = request("success", MeasurementMode::ColdStart);
    adapter.prepare(&request).unwrap();
    let metrics = adapter.sample(&request).unwrap();
    assert!(metrics.elapsed_ns > 0);
    assert_eq!(metrics.peak_rss_bytes, None);
    assert!(
        adapter
            .identity()
            .binary_size_bytes
            .is_some_and(|size| size > 0)
    );
}

#[test]
fn external_adapter_composes_the_main_function_entry() {
    let mut adapter = adapter(Duration::from_secs(1), 1024);
    let mut request = request(
        "// require-main\nfunction main() {}",
        MeasurementMode::ColdStart,
    );
    request.entry = ScriptEntry::MainFunction;
    adapter.prepare(&request).unwrap();
    assert!(adapter.sample(&request).unwrap().elapsed_ns > 0);
}

#[test]
fn external_adapter_rejects_unsupported_and_unprepared_requests() {
    let mut adapter = adapter(Duration::from_secs(1), 1024);
    let unsupported = request("success", MeasurementMode::SteadyState);
    assert_eq!(
        adapter.prepare(&unsupported),
        Err(AdapterError::UnsupportedMode(MeasurementMode::SteadyState))
    );
    let cold = request("success", MeasurementMode::ColdStart);
    assert!(matches!(
        adapter.sample(&cold),
        Err(AdapterError::Setup(message)) if message.contains("before prepare")
    ));

    let mut repeated_cold = cold;
    repeated_cold.iterations = 2;
    assert!(matches!(
        adapter.prepare(&repeated_cold),
        Err(AdapterError::Setup(message)) if message.contains("exactly once")
    ));
}

#[test]
fn external_adapter_caps_failure_output_and_preserves_status() {
    let mut adapter = adapter(Duration::from_secs(1), 16);
    let request = request("failure", MeasurementMode::ColdStart);
    adapter.prepare(&request).unwrap();
    match adapter.sample(&request) {
        Err(AdapterError::Execution {
            status,
            stdout,
            stderr,
        }) => {
            assert_eq!(status, 7);
            assert!(stdout.ends_with("[truncated]"));
            assert_eq!(&*stderr, "fixture stderr");
        }
        outcome => panic!("unexpected outcome: {outcome:?}"),
    }
}

#[test]
fn external_adapter_kills_process_at_deadline() {
    let mut adapter = adapter(Duration::from_millis(20), 1024);
    let request = request("timeout", MeasurementMode::ColdStart);
    adapter.prepare(&request).unwrap();
    let start = std::time::Instant::now();
    assert!(matches!(
        adapter.sample(&request),
        Err(AdapterError::Timeout { stdout, stderr, .. })
            if &*stdout == "timeout stdout" && &*stderr == "timeout stderr"
    ));
    assert!(start.elapsed() < Duration::from_secs(1));
}
