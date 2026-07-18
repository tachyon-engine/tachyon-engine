use std::{hint::black_box, sync::Arc, time::Instant};

use tachyon_bytecode::CompiledModule;
use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};
use tachyon_gc::HeapLimit;
use tachyon_vm::{
    AtomHashSeed, AtomTableConfig, CodeId, ExecutionBudget, Isolate, IsolateConfig, RealmLimits,
    RunOutcome, StackLimits,
};

use crate::{
    AdapterError, BenchmarkAdapter, BenchmarkRequest, EngineIdentity, EngineKind, MeasurementMode,
    SampleMetrics, ScriptEntry, TachyonBenchmarkConfig,
    adapter::{MAIN_INVOCATION_SOURCE, compose_execution_source},
};

/// Fully explicit isolate policy for the in-process Tachyon adapter.
#[derive(Clone, Copy, Debug)]
pub struct TachyonInProcessConfig {
    isolate: IsolateConfig,
}

/// In-process adapter with separate compile and execution timing boundaries.
pub struct TachyonInProcessAdapter {
    identity: EngineIdentity,
    config: TachyonInProcessConfig,
    prepared: Option<PreparedRequest>,
}

struct PreparedRequest {
    script_id: Box<str>,
    source: Arc<str>,
    entry: ScriptEntry,
    execution_source: Arc<str>,
    mode: MeasurementMode,
    iterations: u64,
    module: Option<CompiledModule>,
    code: Option<CodeId>,
    isolate: Isolate,
}

impl TachyonInProcessConfig {
    /// Converts the centralized benchmark harness constants into VM configuration.
    #[must_use]
    pub fn from_benchmark(config: TachyonBenchmarkConfig) -> Self {
        Self {
            isolate: IsolateConfig::new(
                AtomTableConfig::new(
                    config.atom_max_entries,
                    config.atom_max_bytes,
                    AtomHashSeed::new(config.atom_hash_seed_0, config.atom_hash_seed_1),
                ),
                HeapLimit::new(config.heap_max_bytes),
                StackLimits::new(config.stack_max_frames, config.stack_max_registers),
                RealmLimits::new(config.max_loaded_modules, config.max_global_bindings),
            ),
        }
    }
}

impl TachyonInProcessAdapter {
    /// Validates the engine identity and stores the explicit isolate configuration.
    pub fn new(
        identity: EngineIdentity,
        config: TachyonInProcessConfig,
    ) -> Result<Self, AdapterError> {
        if identity.kind != EngineKind::TachyonInProcess {
            return Err(AdapterError::Setup(
                "Tachyon in-process adapter requires TachyonInProcess identity".into(),
            ));
        }
        Ok(Self {
            identity,
            config,
            prepared: None,
        })
    }

    fn verify_prepared(&self, request: &BenchmarkRequest) -> Result<(), AdapterError> {
        let Some(prepared) = &self.prepared else {
            return Err(AdapterError::Setup(
                "Tachyon sample called before prepare".into(),
            ));
        };
        if prepared.script_id != request.script_id
            || prepared.source != request.source
            || prepared.entry != request.entry
            || prepared.mode != request.mode
            || prepared.iterations != request.iterations
        {
            return Err(AdapterError::Setup(
                "Tachyon request differs from prepared source or mode".into(),
            ));
        }
        Ok(())
    }

    fn parse_compile_execute(
        prepared: &mut PreparedRequest,
    ) -> Result<SampleMetrics, AdapterError> {
        let start = Instant::now();
        let module = Compiler
            .compile(
                source_text(Arc::clone(&prepared.execution_source)),
                CompileOptions::default(),
            )
            .map_err(|error| AdapterError::Engine(format!("compile failed: {error:?}").into()))?;
        execute_once(&mut prepared.isolate, &module)?;
        Ok(SampleMetrics {
            elapsed_ns: elapsed_ns(start),
            iterations: 1,
            peak_rss_bytes: None,
        })
    }

    fn precompiled_execute(prepared: &mut PreparedRequest) -> Result<SampleMetrics, AdapterError> {
        let module = prepared
            .module
            .as_ref()
            .ok_or_else(|| AdapterError::Setup("prepared Tachyon module is missing".into()))?;
        let code = prepared
            .code
            .ok_or_else(|| AdapterError::Setup("prepared Tachyon code is missing".into()))?;
        let start = Instant::now();
        black_box(module);
        execute_loaded_once(&mut prepared.isolate, code)?;
        Ok(SampleMetrics {
            elapsed_ns: elapsed_ns(start),
            iterations: 1,
            peak_rss_bytes: None,
        })
    }

    /// Repeats execution inside one timed sample so timer and adapter overhead are amortized explicitly.
    fn steady_state(
        prepared: &mut PreparedRequest,
        iterations: u64,
    ) -> Result<SampleMetrics, AdapterError> {
        let module = prepared
            .module
            .as_ref()
            .ok_or_else(|| AdapterError::Setup("prepared Tachyon module is missing".into()))?;
        let code = prepared
            .code
            .ok_or_else(|| AdapterError::Setup("prepared Tachyon code is missing".into()))?;
        let start = Instant::now();
        black_box(module);
        for _ in 0..iterations {
            execute_loaded_once(&mut prepared.isolate, code)?;
        }
        Ok(SampleMetrics {
            elapsed_ns: elapsed_ns(start),
            iterations,
            peak_rss_bytes: None,
        })
    }
}

impl BenchmarkAdapter for TachyonInProcessAdapter {
    fn identity(&self) -> &EngineIdentity {
        &self.identity
    }

    fn prepare(&mut self, request: &BenchmarkRequest) -> Result<(), AdapterError> {
        if request.mode == MeasurementMode::ColdStart {
            return Err(AdapterError::UnsupportedMode(request.mode));
        }
        if request.iterations == 0
            || (request.mode != MeasurementMode::SteadyState && request.iterations != 1)
        {
            return Err(AdapterError::Setup(
                "Tachyon request has an invalid iteration count for its mode".into(),
            ));
        }
        self.prepared = None;
        let execution_source = compose_execution_source(&request.source, request.entry)?;
        let mut isolate = Isolate::new(self.config.isolate).map_err(|error| {
            AdapterError::Setup(format!("Tachyon isolate creation failed: {error:?}").into())
        })?;
        let (module, code) = match request.mode {
            MeasurementMode::PrecompiledExecute | MeasurementMode::SteadyState => {
                if request.entry == ScriptEntry::MainFunction {
                    let setup = compile_for_prepare(Arc::clone(&request.source))?;
                    let setup_code = load_for_prepare(&mut isolate, &setup)?;
                    execute_setup_once(&mut isolate, setup_code)?;
                }
                let workload_source = if request.entry == ScriptEntry::MainFunction {
                    Arc::from(MAIN_INVOCATION_SOURCE)
                } else {
                    Arc::clone(&request.source)
                };
                let module = compile_for_prepare(workload_source)?;
                let code = load_for_prepare(&mut isolate, &module)?;
                (Some(module), Some(code))
            }
            MeasurementMode::ParseCompileExecute => (None, None),
            MeasurementMode::ColdStart => unreachable!("cold start returned above"),
        };
        self.prepared = Some(PreparedRequest {
            script_id: request.script_id.clone(),
            source: Arc::clone(&request.source),
            entry: request.entry,
            execution_source,
            mode: request.mode,
            iterations: request.iterations,
            module,
            code,
            isolate,
        });
        Ok(())
    }

    fn sample(&mut self, request: &BenchmarkRequest) -> Result<SampleMetrics, AdapterError> {
        if request.mode == MeasurementMode::ColdStart {
            return Err(AdapterError::UnsupportedMode(request.mode));
        }
        self.verify_prepared(request)?;
        let prepared = self
            .prepared
            .as_mut()
            .expect("verified prepared request remains present");
        match request.mode {
            MeasurementMode::ParseCompileExecute => Self::parse_compile_execute(prepared),
            MeasurementMode::PrecompiledExecute => Self::precompiled_execute(prepared),
            MeasurementMode::SteadyState => Self::steady_state(prepared, request.iterations),
            MeasurementMode::ColdStart => unreachable!("cold start returned above"),
        }
    }
}

fn compile_for_prepare(source: Arc<str>) -> Result<CompiledModule, AdapterError> {
    Compiler
        .compile(source_text(source), CompileOptions::default())
        .map_err(|error| AdapterError::Setup(format!("Tachyon compile failed: {error:?}").into()))
}

fn load_for_prepare(
    isolate: &mut Isolate,
    module: &CompiledModule,
) -> Result<CodeId, AdapterError> {
    isolate.load_module(module).map_err(|error| {
        AdapterError::Setup(format!("Tachyon module load failed: {error:?}").into())
    })
}

/// Evaluates corpus setup outside timed samples before resolving the separate `main` invocation.
fn execute_setup_once(isolate: &mut Isolate, code: CodeId) -> Result<(), AdapterError> {
    let outcome = isolate
        .execute_loaded(
            code,
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .map_err(|error| {
            AdapterError::Setup(format!("Tachyon setup execution failed: {error:?}").into())
        })?;
    match outcome {
        RunOutcome::Completed(value) => {
            black_box(value);
            Ok(())
        }
        RunOutcome::Thrown(value) => Err(AdapterError::Setup(
            format!("Tachyon setup threw {value:?}").into(),
        )),
        RunOutcome::BudgetExhausted => Err(AdapterError::Setup(
            "Tachyon setup exhausted an effectively unbounded benchmark budget".into(),
        )),
    }
}

fn source_text(source: Arc<str>) -> SourceText {
    SourceText::new(
        SourceId::new(0),
        SourceName::new("tachyon-benchmark"),
        MediaType::JavaScript,
        source,
    )
}

/// Executes one complete entry job and rejects throws or impossible unbounded-budget suspension.
fn execute_once(isolate: &mut Isolate, module: &CompiledModule) -> Result<(), AdapterError> {
    let outcome = isolate
        .execute(
            module,
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .map_err(|error| AdapterError::Engine(format!("execution failed: {error:?}").into()))?;
    observe_outcome(outcome)
}

/// Executes one previously loaded entry without repeating module identity/name resolution.
fn execute_loaded_once(isolate: &mut Isolate, code: CodeId) -> Result<(), AdapterError> {
    let outcome = isolate
        .execute_loaded(
            code,
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .map_err(|error| AdapterError::Engine(format!("execution failed: {error:?}").into()))?;
    observe_outcome(outcome)
}

fn observe_outcome(outcome: RunOutcome) -> Result<(), AdapterError> {
    match outcome {
        RunOutcome::Completed(value) => {
            black_box(value);
            Ok(())
        }
        RunOutcome::Thrown(value) => Err(AdapterError::Engine(
            format!("Tachyon threw {value:?}").into(),
        )),
        RunOutcome::BudgetExhausted => Err(AdapterError::Engine(
            "Tachyon exhausted an effectively unbounded benchmark budget".into(),
        )),
    }
}

fn elapsed_ns(start: Instant) -> u64 {
    start.elapsed().as_nanos().try_into().unwrap_or(u64::MAX)
}
