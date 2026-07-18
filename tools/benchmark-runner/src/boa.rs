use std::{hint::black_box, sync::Arc, time::Instant};

use boa_engine::{Context, JsObject, JsValue, Source, js_string};

use crate::{
    AdapterError, BenchmarkAdapter, BenchmarkRequest, EngineIdentity, EngineKind, MeasurementMode,
    SampleMetrics, ScriptEntry,
};

/// Boa 0.21 linked into the benchmark process with setup outside steady-state samples.
pub struct BoaInProcessAdapter {
    identity: EngineIdentity,
    prepared: Option<PreparedBoa>,
}

struct PreparedBoa {
    script_id: Box<str>,
    source: Arc<str>,
    iterations: u64,
    context: Context,
    main: JsObject,
}

impl BoaInProcessAdapter {
    /// Accepts only the dedicated Boa in-process identity.
    pub fn new(identity: EngineIdentity) -> Result<Self, AdapterError> {
        if identity.kind != EngineKind::BoaInProcess {
            return Err(AdapterError::Setup(
                "Boa adapter requires BoaInProcess identity".into(),
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
                "Boa sample called before prepare".into(),
            ));
        };
        if prepared.script_id != request.script_id
            || prepared.source != request.source
            || prepared.iterations != request.iterations
        {
            return Err(AdapterError::Setup(
                "Boa request differs from prepared workload".into(),
            ));
        }
        Ok(())
    }
}

impl BenchmarkAdapter for BoaInProcessAdapter {
    fn identity(&self) -> &EngineIdentity {
        &self.identity
    }

    /// Evaluates setup once and resolves the callable `main` before any sample begins.
    fn prepare(&mut self, request: &BenchmarkRequest) -> Result<(), AdapterError> {
        if request.mode != MeasurementMode::SteadyState
            || request.entry != ScriptEntry::MainFunction
        {
            return Err(AdapterError::UnsupportedMode(request.mode));
        }
        if request.iterations == 0 {
            return Err(AdapterError::Setup(
                "Boa steady-state iterations must be nonzero".into(),
            ));
        }
        self.prepared = None;
        let mut context = Context::default();
        context
            .eval(Source::from_bytes(request.source.as_bytes()))
            .map_err(|error| AdapterError::Setup(format!("Boa setup failed: {error}").into()))?;
        let main = context
            .global_object()
            .get(js_string!("main"), &mut context)
            .map_err(|error| {
                AdapterError::Setup(format!("Boa main lookup failed: {error}").into())
            })?
            .as_object()
            .filter(|object| object.is_callable())
            .ok_or_else(|| AdapterError::Setup("Boa setup did not define callable main".into()))?;
        self.prepared = Some(PreparedBoa {
            script_id: request.script_id.clone(),
            source: Arc::clone(&request.source),
            iterations: request.iterations,
            context,
            main,
        });
        Ok(())
    }

    /// Calls the already-resolved JS function exactly `iterations` times inside one sample.
    fn sample(&mut self, request: &BenchmarkRequest) -> Result<SampleMetrics, AdapterError> {
        self.verify_prepared(request)?;
        let prepared = self
            .prepared
            .as_mut()
            .expect("verified Boa request remains prepared");
        let start = Instant::now();
        for _ in 0..request.iterations {
            let value = prepared
                .main
                .call(&JsValue::undefined(), &[], &mut prepared.context)
                .map_err(|error| {
                    AdapterError::Engine(format!("Boa main call failed: {error}").into())
                })?;
            black_box(value);
        }
        Ok(SampleMetrics {
            elapsed_ns: elapsed_ns(start),
            iterations: request.iterations,
            peak_rss_bytes: None,
        })
    }
}

fn elapsed_ns(start: Instant) -> u64 {
    start.elapsed().as_nanos().try_into().unwrap_or(u64::MAX)
}
