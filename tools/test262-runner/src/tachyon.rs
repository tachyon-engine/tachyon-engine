use std::{
    collections::VecDeque,
    future::Future,
    num::NonZeroU32,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    task::{Context, Wake, Waker},
};

use tachyon_compiler::{
    CompileError, CompileOptions, Compiler, DynamicFunctionKind as CompilerDynamicFunctionKind,
    MediaType, SourceId, SourceMode, SourceName, SourceText,
};
use tachyon_gc::HeapLimit;
use tachyon_vm::{
    AtomHashSeed, AtomTableConfig, DynamicFunctionKind, DynamicFunctionSource, ExecutionBudget,
    ExecutionError, HostProviderError, HostProviders, Isolate, IsolateConfig, LoadedModule,
    ModuleError, ModuleIdentity, ModuleLoadError, ModuleLoader, PromiseOutcome, RealmId,
    RealmLimits, ResolvedModuleRequest, RunOutcome, StackLimits, TimeZoneProvider, Value,
    WallClockProvider,
};

use crate::{
    EngineAdapter, EngineOutcome, EngineResponse, ExecutionRequest, Phase, SourceUnit,
    agent::{AgentController, Test262AgentCluster},
};

const ATOM_MAX_ENTRIES: u32 = 1 << 18;
const ATOM_MAX_BYTES: usize = 32 * 1024 * 1024;
// Test262 variants execute in-process, so a bounded per-variant budget keeps one unsupported
// infinite loop from suppressing a complete report. The normative TCO helper performs 100,000
// recursive calls, so the limit must cover several bytecodes per iteration without becoming open.
const EXECUTION_FUEL_LIMIT: u64 = 20_000_000;
// A transition can consume a full interpreter quantum. This permits sixteen scheduler passes for
// every configured module while keeping a live-but-nonterminating job graph bounded independently
// from bytecode instruction fuel.
const MODULE_TRANSITION_LIMIT: u32 = MAX_LOADED_MODULES * 16;
const HEAP_LIMIT_BYTES: usize = 256 * 1024 * 1024;
const STACK_MAX_FRAMES: u32 = 4_096;
const STACK_MAX_REGISTERS: u32 = 2 * 1024 * 1024;
// Dynamic Function/eval tests legitimately compile many short scripts in one isolate.
const MAX_LOADED_MODULES: u32 = 4_096;
const MAX_GLOBAL_BINDINGS: u32 = 1 << 18;
const ASYNC_HARNESS_NAME: &str = "doneprintHandle.js";
const GLOBAL_OBJECT_HARNESS_NAME: &str = "fnGlobalObject.js";
const GLOBAL_OBJECT_HARNESS_SOURCE: &str = r#"
function fnGlobalObject() {
  return globalThis;
}
"#;
const ASYNC_HARNESS_SOURCE: &str = r#"
var __tachyonAsyncStatus = 0;
function $DONE(error) {
  if (__tachyonAsyncStatus !== 0 || error) {
    __tachyonAsyncStatus = 2;
  } else {
    __tachyonAsyncStatus = 1;
  }
}
globalThis.$DONE = $DONE;
"#;
const ASYNC_PROBE_SOURCE: &str = "__tachyonAsyncStatus;";
const AGENT_BOOTSTRAP_SOURCE: &str = r#"
(function (agent) {
  const rawStart = agent._start;
  const rawBroadcast = agent._broadcast;
  const rawReceiveBroadcast = agent._receiveBroadcast;
  const rawReport = agent._report;
  const rawGetReport = agent._getReport;
  const rawSleep = agent._sleep;
  const rawMonotonicNow = agent._monotonicNow;
  const rawLeaving = agent._leaving;

  agent.start = function (source) { return rawStart(String(source)); };
  agent.broadcast = function (sab, value) {
    if (value !== undefined && typeof value !== "bigint") value = Number(value) | 0;
    return rawBroadcast(sab, value);
  };
  agent.receiveBroadcast = function (callback) {
    const packet = rawReceiveBroadcast();
    return callback(packet.buffer, packet.value);
  };
  agent.report = function (message) { return rawReport(String(message)); };
  agent.getReport = function () { return rawGetReport(); };
  agent.sleep = function (milliseconds) { return rawSleep(Number(milliseconds)); };
  agent.monotonicNow = function () { return rawMonotonicNow(); };
  agent.leaving = function () { return rawLeaving(); };
})(globalThis.$262.agent);
"#;

/// Deterministic clock that also permits conformance fixtures to observe forward progress.
#[derive(Default)]
struct Test262WallClock {
    next_millisecond: i64,
}

impl WallClockProvider for Test262WallClock {
    fn unix_time_milliseconds(&mut self) -> Result<i64, HostProviderError> {
        let current = self.next_millisecond;
        self.next_millisecond = self.next_millisecond.saturating_add(1);
        Ok(current)
    }
}

struct Test262UtcTimeZone;

impl TimeZoneProvider for Test262UtcTimeZone {
    fn offset_milliseconds_for_utc(
        &mut self,
        _utc_milliseconds: i64,
    ) -> Result<i64, HostProviderError> {
        Ok(0)
    }

    fn utc_milliseconds_for_local(
        &mut self,
        local_milliseconds: i64,
    ) -> Result<i64, HostProviderError> {
        Ok(local_milliseconds)
    }
}

/// Compiles and executes `$262.evalScript` in the realm selected by the host hook.
fn eval_script_callback(
    isolate: &mut Isolate,
    realm: RealmId,
    kind: tachyon_vm::EvalKind,
    source: Value,
) -> Result<Value, ExecutionError> {
    let units = isolate.string_value_to_utf16(source)?;
    let mut source = String::from_utf16_lossy(&units);
    if kind.inherits_strict() {
        const STRICT_PROLOGUE: &str = "\"use strict\";\nvoid 0;\n";
        source
            .try_reserve_exact(STRICT_PROLOGUE.len())
            .map_err(|_| ExecutionError::UnsupportedDynamicFunctionConstructor)?;
        source.insert_str(0, STRICT_PROLOGUE);
    }
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(u32::MAX - 1),
                SourceName::new("$262.evalScript"),
                MediaType::JavaScript,
                Arc::<str>::from(source),
            ),
            CompileOptions {
                direct_eval: matches!(kind, tachyon_vm::EvalKind::Direct { .. }),
                ..options(SourceMode::Script)
            },
        )
        .map_err(|error| match error {
            CompileError::Diagnostics(_) => ExecutionError::InvalidEvalSource,
            _ => ExecutionError::UnsupportedDynamicFunctionConstructor,
        })?;
    let budget = ExecutionBudget {
        fuel: EXECUTION_FUEL_LIMIT,
        quantum: u32::MAX,
    };
    let outcome = match kind {
        tachyon_vm::EvalKind::Direct { .. } => {
            isolate.execute_direct_eval_in_realm(realm, &module, budget, kind.inherits_strict())
        }
        tachyon_vm::EvalKind::Indirect => isolate.execute_in_realm(realm, &module, budget),
    }?;
    match outcome {
        RunOutcome::Completed(value) => Ok(value),
        RunOutcome::Thrown(value) => Err(ExecutionError::HostThrown(value)),
        RunOutcome::BudgetExhausted => Err(ExecutionError::UnsupportedDynamicFunctionConstructor),
    }
}

/// Compiles the empty dynamic Function used by cross-realm constructor tests.
fn dynamic_function_callback(
    isolate: &mut Isolate,
    realm: RealmId,
    kind: DynamicFunctionKind,
    source: DynamicFunctionSource,
) -> Result<Value, ExecutionError> {
    let kind = match kind {
        DynamicFunctionKind::Ordinary => CompilerDynamicFunctionKind::Ordinary,
        DynamicFunctionKind::Generator => CompilerDynamicFunctionKind::Generator,
        DynamicFunctionKind::Async => CompilerDynamicFunctionKind::Async,
        DynamicFunctionKind::AsyncGenerator => CompilerDynamicFunctionKind::AsyncGenerator,
    };
    let module = Compiler
        .compile_dynamic_function(
            SourceId::new(u32::MAX - 2),
            SourceName::new("Function"),
            kind,
            &source.parameters,
            &source.body,
        )
        .map_err(|error| match error {
            CompileError::Diagnostics(_) => ExecutionError::InvalidEvalSource,
            _ => ExecutionError::UnsupportedDynamicFunctionConstructor,
        })?;
    match isolate.execute_in_realm(
        realm,
        &module,
        ExecutionBudget {
            fuel: EXECUTION_FUEL_LIMIT,
            quantum: u32::MAX,
        },
    )? {
        RunOutcome::Completed(value) => Ok(value),
        RunOutcome::Thrown(_) | RunOutcome::BudgetExhausted => {
            Err(ExecutionError::UnsupportedDynamicFunctionConstructor)
        }
    }
}

/// Stateless in-process Test262 adapter; each request owns an independent Tachyon isolate.
#[derive(Clone, Copy, Debug, Default)]
pub struct TachyonAdapter;

impl EngineAdapter for TachyonAdapter {
    /// Executes one request without translating Rust panics into ECMAScript outcomes.
    fn execute(&self, request: ExecutionRequest<'_>) -> EngineResponse {
        EngineResponse::new(execute_request(request))
    }
}

/// Parses the body before harness lowering, then executes all source units in one isolate.
fn execute_request(request: ExecutionRequest<'_>) -> EngineOutcome {
    let body_mode = if request.is_module {
        SourceMode::Module
    } else {
        SourceMode::Script
    };
    if let Err(error) = Compiler.parse(source_text(0, &request.test.body), options(body_mode)) {
        return body_compile_error(error);
    }
    for (index, prelude) in request.test.preludes.iter().enumerate() {
        if let Err(error) = Compiler.parse(
            prelude_source_text(source_id(index, 1), prelude, request.is_async),
            options(SourceMode::Script),
        ) {
            return unsupported(format!(
                "Tachyon cannot parse Test262 harness `{}`: {}",
                prelude.name,
                compile_error_message(&error)
            ));
        }
    }

    let agent_controller = AgentController::new();
    let mut isolate = match Isolate::new_with_host_providers(
        IsolateConfig::new(
            AtomTableConfig::new(
                ATOM_MAX_ENTRIES,
                ATOM_MAX_BYTES,
                AtomHashSeed::new(0x7461_6368_796f_6e31, 0x7465_7374_3236_3231),
            ),
            HeapLimit::new(HEAP_LIMIT_BYTES),
            StackLimits::new(STACK_MAX_FRAMES, STACK_MAX_REGISTERS),
            RealmLimits::new(MAX_LOADED_MODULES, MAX_GLOBAL_BINDINGS),
        ),
        HostProviders::new()
            .with_wall_clock(Test262WallClock::default())
            .with_time_zone(Test262UtcTimeZone)
            .with_atomics_waiter(agent_controller.waiter())
            .with_agent_host(agent_controller.main_host())
            .with_agent_can_suspend(request.can_block),
    ) {
        Ok(isolate) => isolate,
        Err(error) => return unsupported(format!("Tachyon isolate creation failed: {error:?}")),
    };
    if let Err(error) = isolate.install_realm_hooks(eval_script_callback, dynamic_function_callback)
    {
        return unsupported(format!("Tachyon realm hook installation failed: {error:?}"));
    }
    if let Some(outcome) = install_agent_bootstrap(&mut isolate) {
        return outcome;
    }
    for (index, prelude) in request.test.preludes.iter().enumerate() {
        let module = match Compiler.compile(
            prelude_source_text(source_id(index, 1), prelude, request.is_async),
            options(SourceMode::Script),
        ) {
            Ok(module) => module,
            Err(error) => {
                return unsupported(format!(
                    "Tachyon cannot lower Test262 harness `{}`: {}",
                    prelude.name,
                    compile_error_message(&error)
                ));
            }
        };
        if let Some(outcome) = execute_module(&mut isolate, &module) {
            return outcome;
        }
    }

    let module = match Compiler.compile(source_text(0, &request.test.body), options(body_mode)) {
        Ok(module) => module,
        Err(error) => return body_compile_error(error),
    };
    if request.is_module {
        if let Some(outcome) = execute_source_module(
            &mut isolate,
            &module,
            &request.test.body,
            &request.test.modules,
        ) {
            return outcome;
        }
    } else if let Some(outcome) = execute_module(&mut isolate, &module) {
        return outcome;
    } else {
        let root = match ModuleIdentity::try_new(&request.test.body.name) {
            Ok(identity) => identity,
            Err(error) => return unsupported(format!("invalid script identity: {error:?}")),
        };
        let mut loader = Test262ModuleLoader::for_script(root, &request.test.modules);
        if let Some(outcome) = drive_test262_work(&mut isolate, &mut loader, None) {
            return outcome;
        }
    }
    if request.is_async {
        return execute_async_probe(&mut isolate);
    }
    EngineOutcome::Completed
}

/// Builds and executes the adapter-owned wrappers around raw host-agent native functions.
fn install_agent_bootstrap(isolate: &mut Isolate) -> Option<EngineOutcome> {
    let source = SourceText::new(
        SourceId::new(u32::MAX - 3),
        SourceName::new("test262-agent-bootstrap"),
        MediaType::JavaScript,
        Arc::from(AGENT_BOOTSTRAP_SOURCE),
    );
    let module = match Compiler.compile(source, options(SourceMode::Script)) {
        Ok(module) => module,
        Err(error) => {
            return Some(unsupported(format!(
                "Tachyon cannot lower its agent bootstrap: {}",
                compile_error_message(&error)
            )));
        }
    };
    execute_module(isolate, &module)
}

/// Owns one worker isolate from hook installation through source completion and teardown.
pub(super) fn run_agent_worker(
    source: Box<[u16]>,
    cluster: Arc<Test262AgentCluster>,
    worker: u64,
    ready: mpsc::SyncSender<Result<(), HostProviderError>>,
) {
    let mut isolate = match Isolate::new_with_host_providers(
        IsolateConfig::new(
            AtomTableConfig::new(
                ATOM_MAX_ENTRIES,
                ATOM_MAX_BYTES,
                AtomHashSeed::new(0x7461_6368_796f_6e31, 0x7465_7374_3236_3231),
            ),
            HeapLimit::new(HEAP_LIMIT_BYTES),
            StackLimits::new(STACK_MAX_FRAMES, STACK_MAX_REGISTERS),
            RealmLimits::new(MAX_LOADED_MODULES, MAX_GLOBAL_BINDINGS),
        ),
        HostProviders::new()
            .with_wall_clock(Test262WallClock::default())
            .with_time_zone(Test262UtcTimeZone)
            .with_atomics_waiter(cluster.waiter())
            .with_agent_host(cluster.worker_host(worker))
            .with_agent_can_suspend(true),
    ) {
        Ok(isolate) => isolate,
        Err(_) => {
            let _ = ready.send(Err(HostProviderError::Failure(4)));
            cluster.worker_finished(worker);
            return;
        }
    };
    if isolate
        .install_realm_hooks(eval_script_callback, dynamic_function_callback)
        .is_err()
        || install_agent_bootstrap(&mut isolate).is_some()
    {
        let _ = ready.send(Err(HostProviderError::Failure(5)));
        return;
    }
    let module = match Compiler.compile(
        SourceText::new(
            SourceId::new(u32::MAX - 4),
            SourceName::new("test262-agent-source"),
            MediaType::JavaScript,
            Arc::<str>::from(String::from_utf16_lossy(&source)),
        ),
        options(SourceMode::Script),
    ) {
        Ok(module) => module,
        Err(_) => {
            let _ = ready.send(Err(HostProviderError::Failure(6)));
            return;
        }
    };
    if let Err(error) = cluster.worker_ready(worker) {
        let _ = ready.send(Err(error));
        return;
    }
    if ready.send(Ok(())).is_err() {
        return;
    }
    let _ = execute_module(&mut isolate, &module);
}

fn options(source_mode: SourceMode) -> CompileOptions {
    CompileOptions {
        source_mode,
        ..CompileOptions::default()
    }
}

fn source_id(index: usize, offset: u32) -> u32 {
    u32::try_from(index)
        .ok()
        .and_then(|index| index.checked_add(offset))
        .unwrap_or(u32::MAX)
}

fn source_text(id: u32, unit: &SourceUnit) -> SourceText {
    SourceText::new(
        SourceId::new(id),
        SourceName::new(Arc::<str>::from(&*unit.name)),
        MediaType::JavaScript,
        Arc::clone(&unit.source),
    )
}

/// Replaces the print-based async harness with an isolate-local completion status for this adapter.
fn prelude_source_text(id: u32, unit: &SourceUnit, is_async: bool) -> SourceText {
    if is_async && unit.name.as_ref() == ASYNC_HARNESS_NAME {
        return SourceText::new(
            SourceId::new(id),
            SourceName::new(ASYNC_HARNESS_NAME),
            MediaType::JavaScript,
            Arc::from(ASYNC_HARNESS_SOURCE),
        );
    }
    if unit.name.as_ref() == GLOBAL_OBJECT_HARNESS_NAME {
        return SourceText::new(
            SourceId::new(id),
            SourceName::new(GLOBAL_OBJECT_HARNESS_NAME),
            MediaType::JavaScript,
            Arc::from(GLOBAL_OBJECT_HARNESS_SOURCE),
        );
    }
    source_text(id, unit)
}

/// Reads the completion flag only after the VM has drained the body's Promise checkpoint.
fn execute_async_probe(isolate: &mut Isolate) -> EngineOutcome {
    let source = SourceText::new(
        SourceId::new(u32::MAX),
        SourceName::new("tachyon-async-probe"),
        MediaType::JavaScript,
        Arc::from(ASYNC_PROBE_SOURCE),
    );
    let module = match Compiler.compile(source, options(SourceMode::Script)) {
        Ok(module) => module,
        Err(error) => {
            return unsupported(format!(
                "Tachyon cannot lower its async completion probe: {}",
                compile_error_message(&error)
            ));
        }
    };
    match isolate.execute(
        &module,
        ExecutionBudget {
            fuel: EXECUTION_FUEL_LIMIT,
            quantum: u32::MAX,
        },
    ) {
        Ok(RunOutcome::Completed(value)) if value.as_i32() == Some(1) => EngineOutcome::Completed,
        Ok(RunOutcome::Completed(value)) if value.as_i32() == Some(2) => EngineOutcome::Error {
            phase: Phase::Runtime,
            error_type: "Test262Error".into(),
            message: "Tachyon async test called $DONE with an error or more than once".into(),
        },
        Ok(RunOutcome::Completed(_)) => {
            unsupported("Tachyon async test finished without calling $DONE")
        }
        Ok(RunOutcome::Thrown(value)) => EngineOutcome::Error {
            phase: Phase::Runtime,
            error_type: "Error".into(),
            message: format!("Tachyon async completion probe threw {value:?}").into(),
        },
        Ok(RunOutcome::BudgetExhausted) => EngineOutcome::Timeout {
            message: "Tachyon async completion probe exhausted its instruction budget".into(),
        },
        Err(error) => unsupported(format!(
            "Tachyon could not execute its async completion probe: {error:?}"
        )),
    }
}

/// Loads and evaluates one module graph exclusively from the suite's owned source units.
fn execute_source_module(
    isolate: &mut Isolate,
    module: &tachyon_bytecode::CompiledModule,
    body: &SourceUnit,
    fixtures: &[SourceUnit],
) -> Option<EngineOutcome> {
    let root = match ModuleIdentity::try_new(&body.name) {
        Ok(identity) => identity,
        Err(error) => return Some(unsupported(format!("invalid module identity: {error:?}"))),
    };
    let mut loader = Test262ModuleLoader::new(root.clone(), module.clone(), fixtures);
    let root_id = match isolate.load_module_graph(&mut loader, &root) {
        Ok(root_id) => root_id,
        Err(error) => return Some(classify_module_load_error(error)),
    };
    let evaluation = match isolate.evaluate_module_promise(root_id) {
        Ok(promise) => promise,
        Err(error) => {
            return Some(unsupported(format!(
                "module evaluation startup failed: {error:?}"
            )));
        }
    };
    drive_test262_work(isolate, &mut loader, Some(evaluation))
}

/// Pumps the unified VM driver and services owned dynamic-import requests between polls.
fn drive_test262_work(
    isolate: &mut Isolate,
    loader: &mut Test262ModuleLoader,
    root_evaluation: Option<Value>,
) -> Option<EngineOutcome> {
    let wake = Arc::new(DriverWake::default());
    let waker = Waker::from(Arc::clone(&wake));
    let mut context = Context::from_waker(&waker);
    let mut targets = root_evaluation.into_iter().collect::<VecDeque<_>>();
    let quantum = NonZeroU32::new(u32::MAX).expect("driver quantum is non-zero");
    for _ in 0..MODULE_TRANSITION_LIMIT {
        while let Some(request) = isolate.take_pending_dynamic_import() {
            let import_promise = match isolate.load_dynamic_import_graph(loader, &request) {
                Ok(module) => match isolate.complete_dynamic_import_success(request.id(), module) {
                    Ok(promise) => promise,
                    Err(error) => {
                        return Some(unsupported(format!(
                            "dynamic import completion failed: {error:?}"
                        )));
                    }
                },
                Err(_) => {
                    let reason = match isolate
                        .create_native_error(tachyon_vm::NativeErrorKind::Type, None)
                    {
                        Ok(reason) => reason,
                        Err(error) => {
                            return Some(unsupported(format!(
                                "dynamic import error allocation failed: {error:?}"
                            )));
                        }
                    };
                    match isolate.complete_dynamic_import_failure(request.id(), reason) {
                        Ok(promise) => promise,
                        Err(error) => {
                            return Some(unsupported(format!(
                                "dynamic import rejection failed: {error:?}"
                            )));
                        }
                    }
                }
            };
            targets.push_back(import_promise);
        }
        let Some(&target) = targets.front() else {
            match isolate.drive_jobs_once(quantum) {
                Ok(true) => continue,
                Ok(false) => return None,
                Err(error) => {
                    return Some(unsupported(format!("module job drain failed: {error:?}")));
                }
            }
        };
        wake.0.store(false, Ordering::Relaxed);
        let poll = {
            let mut driver = match isolate.drive_promise(target, quantum) {
                Ok(driver) => driver,
                Err(error) => {
                    return Some(unsupported(format!("module driver failed: {error:?}")));
                }
            };
            Pin::new(&mut driver).poll(&mut context)
        };
        match poll {
            core::task::Poll::Ready(Ok(PromiseOutcome::Fulfilled(_))) => {
                targets.pop_front();
            }
            core::task::Poll::Ready(Ok(PromiseOutcome::Rejected(reason))) => {
                if root_evaluation == Some(target) {
                    return classify_run_outcome(isolate, RunOutcome::Thrown(reason));
                }
                targets.pop_front();
            }
            core::task::Poll::Ready(Err(error)) => {
                return Some(unsupported(format!("module driver failed: {error:?}")));
            }
            core::task::Poll::Pending if !wake.0.load(Ordering::Relaxed) => {
                return Some(EngineOutcome::Timeout {
                    message: "Tachyon module driver is quiescent with a pending Promise".into(),
                });
            }
            core::task::Poll::Pending => {}
        }
    }
    Some(EngineOutcome::Timeout {
        message: format!(
            "Tachyon module driver exhausted the {MODULE_TRANSITION_LIMIT} transition budget"
        )
        .into(),
    })
}

#[derive(Default)]
struct DriverWake(AtomicBool);

impl Wake for DriverWake {
    #[inline]
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::Relaxed);
    }

    #[inline]
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Deterministic source-module loader with no filesystem or ambient host capabilities.
struct Test262ModuleLoader {
    root: ModuleIdentity,
    root_module: Option<tachyon_bytecode::CompiledModule>,
    sources: Vec<SourceUnit>,
}

#[derive(Debug)]
enum Test262ModuleLoaderError {
    Compile(CompileError),
    Invalid(Box<str>),
}

impl Test262ModuleLoader {
    fn new(
        root: ModuleIdentity,
        root_module: tachyon_bytecode::CompiledModule,
        fixtures: &[SourceUnit],
    ) -> Self {
        Self {
            root,
            root_module: Some(root_module),
            sources: fixtures.to_vec(),
        }
    }

    fn for_script(root: ModuleIdentity, fixtures: &[SourceUnit]) -> Self {
        Self {
            root,
            root_module: None,
            sources: fixtures.to_vec(),
        }
    }
}

impl ModuleLoader for Test262ModuleLoader {
    type Error = Test262ModuleLoaderError;

    fn resolve(
        &mut self,
        request: &tachyon_bytecode::ModuleRequest,
        referrer: Option<&ModuleIdentity>,
    ) -> Result<ModuleIdentity, Self::Error> {
        let specifier = String::from_utf16(request.specifier.as_ref()).map_err(|_| {
            Test262ModuleLoaderError::Invalid(
                "module request contains an invalid UTF-16 sequence".into(),
            )
        })?;
        let identity = if specifier.starts_with("./") || specifier.starts_with("../") {
            let referrer = referrer.unwrap_or(&self.root);
            let referrer = String::from_utf8(referrer.as_bytes().to_vec()).map_err(|_| {
                Test262ModuleLoaderError::Invalid("module referrer is not UTF-8".into())
            })?;
            normalize_module_path(&referrer, &specifier)
                .map_err(Test262ModuleLoaderError::Invalid)?
        } else {
            specifier
        };
        ModuleIdentity::try_new(&identity)
            .map_err(|error| Test262ModuleLoaderError::Invalid(format!("{error:?}").into()))
    }

    fn load(
        &mut self,
        resolved: ResolvedModuleRequest<'_>,
    ) -> Result<Option<LoadedModule>, Self::Error> {
        if resolved.identity() == &self.root {
            return Ok(self.root_module.take().map(LoadedModule::precompiled));
        }
        let Some(source) = self
            .sources
            .iter()
            .find(|source| source.name.as_bytes() == resolved.identity().as_bytes())
        else {
            return Ok(None);
        };
        let compiled = Compiler
            .compile(
                source_text(source_id(source.name.len(), 100), source),
                options(SourceMode::Module),
            )
            .map_err(Test262ModuleLoaderError::Compile)?;
        Ok(Some(LoadedModule::precompiled(compiled)))
    }
}

/// Converts dependency parse/link failures into Test262's Resolution phase.
fn classify_module_load_error(error: ModuleLoadError<Test262ModuleLoaderError>) -> EngineOutcome {
    match error {
        ModuleLoadError::Graph(ModuleError::MissingExport | ModuleError::AmbiguousExport)
        | ModuleLoadError::Loader(Test262ModuleLoaderError::Compile(CompileError::Diagnostics(
            _,
        ))) => EngineOutcome::Error {
            phase: Phase::Resolution,
            error_type: "SyntaxError".into(),
            message: "Tachyon module resolution failed".into(),
        },
        ModuleLoadError::Loader(Test262ModuleLoaderError::Invalid(message)) => {
            unsupported(format!("module loader rejected request: {message}"))
        }
        other => unsupported(format!("module load failed: {other:?}")),
    }
}

/// Resolves a relative Test262 fixture path without consulting the filesystem.
fn normalize_module_path(referrer: &str, specifier: &str) -> Result<String, Box<str>> {
    let mut components = referrer.split('/').collect::<Vec<_>>();
    components.pop();
    for component in specifier.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components
                    .pop()
                    .ok_or_else(|| Box::<str>::from("module path escapes checkout root"))?;
            }
            value => components.push(value),
        }
    }
    Ok(components.join("/"))
}

/// Maps VM outcomes without treating missing exception objects as a successful test.
fn classify_run_outcome(isolate: &mut Isolate, outcome: RunOutcome) -> Option<EngineOutcome> {
    match outcome {
        RunOutcome::Completed(_) => None,
        RunOutcome::Thrown(value) => {
            let kind = match isolate.native_error_kind(value) {
                Ok(Some(kind)) => kind,
                Ok(None) => {
                    return Some(EngineOutcome::Error {
                        phase: Phase::Runtime,
                        error_type: "Error".into(),
                        message: format!("Tachyon threw non-native value {value:?}").into(),
                    });
                }
                Err(error) => {
                    return Some(unsupported(format!(
                        "Tachyon could not classify thrown value: {error:?}"
                    )));
                }
            };
            Some(EngineOutcome::Error {
                phase: Phase::Runtime,
                error_type: kind.as_str().into(),
                message: format!("Tachyon threw native {}", kind.as_str()).into(),
            })
        }
        RunOutcome::BudgetExhausted => Some(EngineOutcome::Timeout {
            message: format!("Tachyon exhausted the {EXECUTION_FUEL_LIMIT} instruction fuel limit")
                .into(),
        }),
    }
}

/// Executes one ordinary script-compiled module through the bounded adapter entry point.
fn execute_module(
    isolate: &mut Isolate,
    module: &tachyon_bytecode::CompiledModule,
) -> Option<EngineOutcome> {
    let outcome = match isolate.execute(
        module,
        ExecutionBudget {
            fuel: EXECUTION_FUEL_LIMIT,
            quantum: u32::MAX,
        },
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            return Some(unsupported(format!(
                "Tachyon VM does not support test: {error:?}"
            )));
        }
    };
    classify_run_outcome(isolate, outcome)
}

fn body_compile_error(error: CompileError) -> EngineOutcome {
    match error {
        CompileError::Diagnostics(diagnostics) => EngineOutcome::Error {
            phase: Phase::Parse,
            error_type: "SyntaxError".into(),
            message: diagnostics
                .first()
                .map_or_else(
                    || "Tachyon parse failed".into(),
                    |item| item.message.clone(),
                )
                .to_string()
                .into(),
        },
        other => unsupported(format!(
            "Tachyon does not support test source: {}",
            compile_error_message(&other)
        )),
    }
}

fn compile_error_message(error: &CompileError) -> String {
    format!("{error:?}")
}

fn unsupported(reason: impl Into<Box<str>>) -> EngineOutcome {
    EngineOutcome::Unsupported {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{TachyonAdapter, Test262WallClock};
    use crate::{
        ComposedTest, EngineAdapter, EngineOutcome, ExecutionRequest, Phase, SourceUnit,
        TestVariant, VariantKind,
    };
    use tachyon_vm::WallClockProvider;

    /// Builds one content-independent adapter fixture without touching the checkout or filesystem.
    fn composed(body: &str, preludes: &[(&str, &str)], is_async: bool) -> ComposedTest {
        ComposedTest {
            variant: TestVariant {
                kind: VariantKind::Raw,
                is_async,
                can_block: false,
                use_harness: !preludes.is_empty(),
            },
            preludes: preludes
                .iter()
                .map(|(name, source)| SourceUnit {
                    name: (*name).into(),
                    source: (*source).into(),
                })
                .collect(),
            body: SourceUnit {
                name: "test.js".into(),
                source: body.into(),
            },
            modules: Vec::new(),
            source_sha256: "fixture".into(),
        }
    }

    fn execute(test: &ComposedTest) -> EngineOutcome {
        TachyonAdapter
            .execute(ExecutionRequest {
                test,
                can_block: test.variant.can_block,
                is_module: false,
                is_async: test.variant.is_async,
            })
            .outcome
    }

    #[test]
    fn deterministic_wall_clock_advances_without_host_time() {
        let mut clock = Test262WallClock::default();
        assert_eq!(clock.unix_time_milliseconds(), Ok(0));
        assert_eq!(clock.unix_time_milliseconds(), Ok(1));
        assert_eq!(clock.unix_time_milliseconds(), Ok(2));
    }

    #[test]
    fn missing_dynamic_import_is_bounded_by_module_transitions() {
        let outcome = execute(&composed(
            "import('./missing_FIXTURE.js').catch(() => {}).then($DONE, $DONE);",
            &[("doneprintHandle.js", "ignored by Tachyon")],
            true,
        ));
        assert!(matches!(outcome, EngineOutcome::Timeout { .. }));
    }

    /// Proves module variants use the in-memory fixture graph and classify link failures correctly.
    #[test]
    fn adapter_loads_module_fixtures_without_ambient_io() {
        let mut test = composed(
            "import { value } from './dependency_FIXTURE.js'; value;",
            &[],
            false,
        );
        test.body.name = "test/module/root.js".into();
        test.set_modules(vec![SourceUnit {
            name: "test/module/dependency_FIXTURE.js".into(),
            source: "export const value = 42;".into(),
        }]);
        let execute_module = |test: &ComposedTest| {
            TachyonAdapter
                .execute(ExecutionRequest {
                    test,
                    can_block: false,
                    is_module: true,
                    is_async: false,
                })
                .outcome
        };
        assert_eq!(execute_module(&test), EngineOutcome::Completed);

        test.body.source = "import { missing } from './dependency_FIXTURE.js';".into();
        assert!(matches!(
            execute_module(&test),
            EngineOutcome::Error {
                phase: Phase::Resolution,
                ref error_type,
                ..
            } if error_type.as_ref() == "SyntaxError"
        ));
    }

    #[test]
    fn raw_arithmetic_executes_in_tachyon() {
        assert_eq!(
            execute(&composed("1 + 2;", &[], false)),
            EngineOutcome::Completed
        );
    }

    #[test]
    /// Covers source/message conversion, SAB identity, report FIFO, and normal worker teardown.
    fn adapter_agents_share_sab_and_report_without_leaking_workers() {
        let mut test = composed(
            r#"
            $262.agent.start(`
              $262.agent.receiveBroadcast(function (sab) {
                const values = new Int32Array(sab);
                Atomics.store(values, 0, 42);
                $262.agent.report(Atomics.load(values, 0));
                $262.agent.leaving();
              });
            `);
            const values = new Int32Array(new SharedArrayBuffer(4));
            $262.agent.broadcast(values.buffer);
            let report;
            while ((report = $262.agent.getReport()) === null) {
              $262.agent.sleep(1);
            }
            if (report !== "42" || Atomics.load(values, 0) !== 42) {
              throw new Error("agent coordination failed");
            }
            "#,
            &[],
            false,
        );
        test.variant.can_block = true;
        assert_eq!(execute(&test), EngineOutcome::Completed);
    }

    #[test]
    /// Proves request teardown cancels an infinite wait before joining the worker thread.
    fn adapter_cancels_and_joins_blocked_agents_on_request_teardown() {
        let mut test = composed(
            r#"
            $262.agent.start(`
              $262.agent.receiveBroadcast(function (sab) {
                const values = new Int32Array(sab);
                Atomics.wait(values, 0, 0);
                $262.agent.leaving();
              });
            `);
            const values = new Int32Array(new SharedArrayBuffer(4));
            $262.agent.broadcast(values.buffer);
            "#,
            &[],
            false,
        );
        test.variant.can_block = true;
        assert_eq!(execute(&test), EngineOutcome::Completed);
    }

    #[test]
    fn body_syntax_errors_keep_the_parse_phase() {
        assert!(matches!(
            execute(&composed("const = ;", &[], false)),
            EngineOutcome::Error { phase: crate::Phase::Parse, ref error_type, .. }
                if &**error_type == "SyntaxError"
        ));
    }

    #[test]
    fn native_runtime_errors_keep_their_ecmascript_type() {
        assert!(matches!(
            execute(&composed("'use strict'; missing = 1;", &[], false)),
            EngineOutcome::Error { phase: crate::Phase::Runtime, ref error_type, .. }
                if &**error_type == "ReferenceError"
        ));
    }

    #[test]
    fn control_flow_harness_executes_in_tachyon() {
        assert_eq!(
            execute(&composed(
                "assert();",
                &[("assert.js", "function assert() { if (true) {} }")],
                false
            )),
            EngineOutcome::Completed
        );
    }

    #[test]
    fn harness_reference_errors_and_async_done_status_are_explicit() {
        assert!(matches!(
            execute(&composed(
                "1 + 2;",
                &[("assert.js", "assert.value;")],
                false
            )),
            EngineOutcome::Error { phase: crate::Phase::Runtime, ref error_type, .. }
                if &**error_type == "ReferenceError"
        ));
        assert!(matches!(
            execute(&composed("1 + 2;", &[], true)),
            EngineOutcome::Error {
                phase: crate::Phase::Runtime,
                ..
            }
        ));
        assert_eq!(
            execute(&composed(
                "if (!Object.prototype.hasOwnProperty.call(globalThis, '$DONE')) { $DONE('missing own property'); } else { $DONE(); }",
                &[("doneprintHandle.js", "ignored by Tachyon")],
                true,
            )),
            EngineOutcome::Completed
        );
        assert!(matches!(
            execute(&composed(
                "$DONE('failure');",
                &[("doneprintHandle.js", "ignored by Tachyon")],
                true,
            )),
            EngineOutcome::Error {
                phase: crate::Phase::Runtime,
                ref error_type,
                ..
            } if &**error_type == "Test262Error"
        ));
    }

    #[test]
    fn non_async_done_harness_is_not_replaced() {
        assert_eq!(
            execute(&composed(
                "if ($DONE !== 7) { throw new TypeError(); }",
                &[("doneprintHandle.js", "var $DONE = 7;")],
                false,
            )),
            EngineOutcome::Completed
        );
    }
}
