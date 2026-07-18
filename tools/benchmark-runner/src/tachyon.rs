use std::{hint::black_box, sync::Arc, time::Instant};

use tachyon_bytecode::CompiledModule;
use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};
use tachyon_gc::HeapLimit;
use tachyon_vm::{
    AtomHashSeed, AtomTableConfig, ExecutionBudget, Isolate, IsolateConfig, RealmLimits,
    RunOutcome, StackLimits,
};

use crate::{
    AdapterError, BenchmarkAdapter, BenchmarkRequest, EngineIdentity, EngineKind, MeasurementMode,
    SampleMetrics, TachyonBenchmarkConfig,
};

/// Fully explicit isolate and repetition policy for the in-process Tachyon adapter.
#[derive(Clone, Copy, Debug)]
pub struct TachyonInProcessConfig {
    isolate: IsolateConfig,
    steady_state_iterations: u64,
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
    mode: MeasurementMode,
    module: Option<CompiledModule>,
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
            steady_state_iterations: config.steady_state_iterations,
        }
    }
}

impl TachyonInProcessAdapter {
    /// Validates the engine identity and stores an allocation-free steady-state iteration count.
    pub fn new(
        identity: EngineIdentity,
        config: TachyonInProcessConfig,
    ) -> Result<Self, AdapterError> {
        if identity.kind != EngineKind::TachyonInProcess {
            return Err(AdapterError::Setup(
                "Tachyon in-process adapter requires TachyonInProcess identity".into(),
            ));
        }
        if config.steady_state_iterations < 2 {
            return Err(AdapterError::Setup(
                "steady-state mode requires at least two iterations".into(),
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
            || prepared.mode != request.mode
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
                source_text(Arc::clone(&prepared.source)),
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
        let start = Instant::now();
        execute_once(&mut prepared.isolate, module)?;
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
        let start = Instant::now();
        for _ in 0..iterations {
            execute_once(&mut prepared.isolate, module)?;
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
        self.prepared = None;
        // Compilation belongs in prepare only when the selected timing boundary excludes it.
        let module = match request.mode {
            MeasurementMode::PrecompiledExecute | MeasurementMode::SteadyState => Some(
                Compiler
                    .compile(
                        source_text(Arc::clone(&request.source)),
                        CompileOptions::default(),
                    )
                    .map_err(|error| {
                        AdapterError::Setup(format!("Tachyon compile failed: {error:?}").into())
                    })?,
            ),
            MeasurementMode::ParseCompileExecute => None,
            MeasurementMode::ColdStart => unreachable!("cold start returned above"),
        };
        self.prepared = Some(PreparedRequest {
            script_id: request.script_id.clone(),
            source: Arc::clone(&request.source),
            mode: request.mode,
            module,
            isolate: Isolate::new(self.config.isolate).map_err(|error| {
                AdapterError::Setup(format!("Tachyon isolate creation failed: {error:?}").into())
            })?,
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
            MeasurementMode::SteadyState => {
                Self::steady_state(prepared, self.config.steady_state_iterations)
            }
            MeasurementMode::ColdStart => unreachable!("cold start returned above"),
        }
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
    match isolate
        .execute(
            module,
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .map_err(|error| AdapterError::Engine(format!("execution failed: {error:?}").into()))?
    {
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
