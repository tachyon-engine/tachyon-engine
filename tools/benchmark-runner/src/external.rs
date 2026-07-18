use std::{
    ffi::OsString,
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use tempfile::{Builder, NamedTempFile};
use wait_timeout::ChildExt;

use crate::{
    AdapterError, BenchmarkAdapter, BenchmarkRequest, EngineIdentity, EngineKind, MeasurementMode,
    SampleMetrics, ScriptEntry, adapter::compose_execution_source,
};

/// Host-side configuration for a CLI engine whose final argument is a JavaScript file path.
#[derive(Clone, Debug)]
pub struct ExternalProcessConfig {
    /// Exact release executable.
    pub executable: PathBuf,
    /// Arguments inserted before the generated script path, without shell parsing.
    pub fixed_arguments: Vec<OsString>,
    /// Hard deadline for one cold-start sample.
    pub timeout: Duration,
    /// Per-stream diagnostic capture limit.
    pub maximum_output_bytes: usize,
}

/// Serial Escargot CLI adapter that honestly exposes cold-start timing only.
pub struct ExternalProcessAdapter {
    identity: EngineIdentity,
    config: ExternalProcessConfig,
    source_file: NamedTempFile,
    stdout_file: NamedTempFile,
    stderr_file: NamedTempFile,
    prepared: Option<PreparedRequest>,
}

struct PreparedRequest {
    script_id: Box<str>,
    source: Arc<str>,
    entry: ScriptEntry,
    iterations: u64,
}

impl ExternalProcessAdapter {
    /// Validates the release executable and allocates reusable source/output files outside samples.
    pub fn new(
        mut identity: EngineIdentity,
        config: ExternalProcessConfig,
    ) -> Result<Self, AdapterError> {
        if identity.kind != EngineKind::EscargotCli {
            return Err(AdapterError::Setup(
                "external process adapter requires EscargotCli identity".into(),
            ));
        }
        if config.timeout.is_zero() || config.maximum_output_bytes == 0 {
            return Err(AdapterError::Setup(
                "external process timeout and output limit must be nonzero".into(),
            ));
        }
        let metadata = fs::metadata(&config.executable).map_err(|error| {
            AdapterError::Setup(
                format!(
                    "cannot inspect executable {}: {error}",
                    config.executable.display()
                )
                .into(),
            )
        })?;
        if !metadata.is_file() {
            return Err(AdapterError::Setup(
                format!("executable is not a file: {}", config.executable.display()).into(),
            ));
        }
        identity.binary_size_bytes = Some(metadata.len());
        Ok(Self {
            identity,
            config,
            source_file: temporary_file("tachyon-benchmark-source-", ".js")?,
            stdout_file: temporary_file("tachyon-benchmark-stdout-", ".log")?,
            stderr_file: temporary_file("tachyon-benchmark-stderr-", ".log")?,
            prepared: None,
        })
    }

    /// Ensures a sample cannot accidentally execute source different from the prepared request.
    fn verify_prepared(&self, request: &BenchmarkRequest) -> Result<(), AdapterError> {
        let Some(prepared) = &self.prepared else {
            return Err(AdapterError::Setup(
                "external process sample called before prepare".into(),
            ));
        };
        if prepared.script_id != request.script_id
            || prepared.source != request.source
            || prepared.entry != request.entry
            || prepared.iterations != request.iterations
        {
            return Err(AdapterError::Setup(
                "external process request differs from prepared source".into(),
            ));
        }
        Ok(())
    }

    /// Spawns one process, enforces its deadline, and converts bounded diagnostics into adapter outcomes.
    fn cold_start_sample(&mut self) -> Result<SampleMetrics, AdapterError> {
        reset_file(&mut self.stdout_file)?;
        reset_file(&mut self.stderr_file)?;
        let stdout = self.stdout_file.reopen().map_err(setup_io_error)?;
        let stderr = self.stderr_file.reopen().map_err(setup_io_error)?;
        let start = Instant::now();
        let mut child = Command::new(&self.config.executable)
            .args(&self.config.fixed_arguments)
            .arg(self.source_file.path())
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|error| {
                AdapterError::Setup(format!("failed to spawn benchmark engine: {error}").into())
            })?;
        let status = match child.wait_timeout(self.config.timeout).map_err(|error| {
            AdapterError::Setup(format!("failed to wait for engine: {error}").into())
        })? {
            Some(status) => status,
            None => {
                let kill_error = child.kill().err();
                child.wait().map_err(|error| {
                    AdapterError::Setup(format!("failed to reap timed-out engine: {error}").into())
                })?;
                let stdout = read_limited(&self.stdout_file, self.config.maximum_output_bytes)?;
                let stderr = read_limited(&self.stderr_file, self.config.maximum_output_bytes)?;
                let message = kill_error.map_or_else(
                    || format!("engine exceeded {:?}", self.config.timeout),
                    |error| {
                        format!(
                            "engine exceeded {:?}; termination reported {error}",
                            self.config.timeout
                        )
                    },
                );
                return Err(AdapterError::Timeout {
                    message: message.into(),
                    stdout,
                    stderr,
                });
            }
        };
        let elapsed_ns = start.elapsed().as_nanos().try_into().unwrap_or(u64::MAX);
        let stdout = read_limited(&self.stdout_file, self.config.maximum_output_bytes)?;
        let stderr = read_limited(&self.stderr_file, self.config.maximum_output_bytes)?;
        if status.success() {
            return Ok(SampleMetrics {
                elapsed_ns,
                iterations: 1,
                peak_rss_bytes: None,
            });
        }
        match status.code() {
            Some(status) => Err(AdapterError::Execution {
                status,
                stdout,
                stderr,
            }),
            None => Err(AdapterError::Crash {
                message: "engine terminated without an exit status".into(),
                stdout,
                stderr,
            }),
        }
    }
}

impl BenchmarkAdapter for ExternalProcessAdapter {
    fn identity(&self) -> &EngineIdentity {
        &self.identity
    }

    fn prepare(&mut self, request: &BenchmarkRequest) -> Result<(), AdapterError> {
        if request.mode != MeasurementMode::ColdStart {
            return Err(AdapterError::UnsupportedMode(request.mode));
        }
        if request.iterations != 1 {
            return Err(AdapterError::Setup(
                "external cold-start request must execute exactly once".into(),
            ));
        }
        reset_file(&mut self.source_file)?;
        let execution_source = compose_execution_source(&request.source, request.entry)?;
        self.source_file
            .write_all(execution_source.as_bytes())
            .map_err(setup_io_error)?;
        self.source_file.flush().map_err(setup_io_error)?;
        self.prepared = Some(PreparedRequest {
            script_id: request.script_id.clone(),
            source: Arc::clone(&request.source),
            entry: request.entry,
            iterations: request.iterations,
        });
        Ok(())
    }

    fn sample(&mut self, request: &BenchmarkRequest) -> Result<SampleMetrics, AdapterError> {
        if request.mode != MeasurementMode::ColdStart {
            return Err(AdapterError::UnsupportedMode(request.mode));
        }
        self.verify_prepared(request)?;
        self.cold_start_sample()
    }
}

fn temporary_file(prefix: &str, suffix: &str) -> Result<NamedTempFile, AdapterError> {
    Builder::new()
        .prefix(prefix)
        .suffix(suffix)
        .tempfile()
        .map_err(setup_io_error)
}

fn reset_file(file: &mut NamedTempFile) -> Result<(), AdapterError> {
    file.as_file_mut().set_len(0).map_err(setup_io_error)?;
    file.as_file_mut()
        .seek(SeekFrom::Start(0))
        .map_err(setup_io_error)?;
    Ok(())
}

/// Reads at most one byte beyond the cap so truncation is explicit without unbounded allocation.
fn read_limited(file: &NamedTempFile, limit: usize) -> Result<Box<str>, AdapterError> {
    let byte_limit = limit.checked_add(1).ok_or_else(|| {
        AdapterError::Setup("external process output limit cannot be represented".into())
    })?;
    let mut reader = file
        .reopen()
        .map_err(setup_io_error)?
        .take(byte_limit as u64);
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(byte_limit)
        .map_err(|_| AdapterError::Setup("cannot reserve diagnostic output buffer".into()))?;
    reader.read_to_end(&mut bytes).map_err(setup_io_error)?;
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    let mut output = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        output.push_str("\n[truncated]");
    }
    Ok(output.into())
}

fn setup_io_error(error: std::io::Error) -> AdapterError {
    AdapterError::Setup(format!("external process I/O failed: {error}").into())
}
