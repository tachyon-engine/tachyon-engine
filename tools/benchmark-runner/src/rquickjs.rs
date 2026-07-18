use std::{hint::black_box, sync::Arc, time::Instant};

use rquickjs::{Context, Function, Runtime, Value};

use crate::{
    AdapterError, BenchmarkAdapter, BenchmarkRequest, EngineIdentity, EngineKind, MeasurementMode,
    SampleMetrics, ScriptEntry,
};

/// QuickJS linked through rquickjs 0.12 with one runtime/context per prepared case.
pub struct RQuickJsInProcessAdapter {
    identity: EngineIdentity,
    prepared: Option<PreparedRQuickJs>,
}

struct PreparedRQuickJs {
    script_id: Box<str>,
    source: Arc<str>,
    iterations: u64,
    _runtime: Runtime,
    context: Context,
}

impl RQuickJsInProcessAdapter {
    /// Accepts only the dedicated rquickjs in-process identity.
    pub fn new(identity: EngineIdentity) -> Result<Self, AdapterError> {
        if identity.kind != EngineKind::RQuickJsInProcess {
            return Err(AdapterError::Setup(
                "rquickjs adapter requires RQuickJsInProcess identity".into(),
            ));
        }
        Ok(Self {
            identity,
            prepared: None,
        })
    }

    fn verify_prepared(&self, request: &BenchmarkRequest) -> Result<(), AdapterError> {
        let Some(prepared) = &self.prepared else {
            return Err(AdapterError::Setup(
                "rquickjs sample called before prepare".into(),
            ));
        };
        if prepared.script_id != request.script_id
            || prepared.source != request.source
            || prepared.iterations != request.iterations
        {
            return Err(AdapterError::Setup(
                "rquickjs request differs from prepared workload".into(),
            ));
        }
        Ok(())
    }
}

impl BenchmarkAdapter for RQuickJsInProcessAdapter {
    fn identity(&self) -> &EngineIdentity {
        &self.identity
    }

    /// Creates a full QuickJS context, evaluates setup, and validates `main` outside samples.
    fn prepare(&mut self, request: &BenchmarkRequest) -> Result<(), AdapterError> {
        if request.mode != MeasurementMode::SteadyState
            || request.entry != ScriptEntry::MainFunction
        {
            return Err(AdapterError::UnsupportedMode(request.mode));
        }
        if request.iterations == 0 {
            return Err(AdapterError::Setup(
                "rquickjs steady-state iterations must be nonzero".into(),
            ));
        }
        self.prepared = None;
        let runtime = Runtime::new().map_err(rquickjs_setup_error)?;
        let context = Context::full(&runtime).map_err(rquickjs_setup_error)?;
        context
            .with(|ctx| {
                ctx.eval::<(), _>(request.source.as_bytes())?;
                let _: Function<'_> = ctx.globals().get("main")?;
                rquickjs::Result::Ok(())
            })
            .map_err(rquickjs_setup_error)?;
        self.prepared = Some(PreparedRQuickJs {
            script_id: request.script_id.clone(),
            source: Arc::clone(&request.source),
            iterations: request.iterations,
            _runtime: runtime,
            context,
        });
        Ok(())
    }

    /// Resolves `main` under one runtime lock, then times only repeated QuickJS calls.
    fn sample(&mut self, request: &BenchmarkRequest) -> Result<SampleMetrics, AdapterError> {
        self.verify_prepared(request)?;
        let prepared = self
            .prepared
            .as_ref()
            .expect("verified rquickjs request remains prepared");
        prepared
            .context
            .with(|ctx| {
                let main: Function<'_> = ctx.globals().get("main")?;
                let start = Instant::now();
                for _ in 0..request.iterations {
                    let value: Value<'_> = main.call(())?;
                    black_box(value);
                }
                rquickjs::Result::Ok(SampleMetrics {
                    elapsed_ns: elapsed_ns(start),
                    iterations: request.iterations,
                    peak_rss_bytes: None,
                })
            })
            .map_err(|error| {
                AdapterError::Engine(format!("rquickjs main call failed: {error}").into())
            })
    }
}

fn rquickjs_setup_error(error: rquickjs::Error) -> AdapterError {
    AdapterError::Setup(format!("rquickjs setup failed: {error}").into())
}

fn elapsed_ns(start: Instant) -> u64 {
    start.elapsed().as_nanos().try_into().unwrap_or(u64::MAX)
}
