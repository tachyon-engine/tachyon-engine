use std::sync::{Arc, Mutex};

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{
    fixtures::{test_isolate, test_isolate_with_heap_spans},
    *,
};

#[derive(Clone)]
struct RecordingWaiterProvider {
    calls: Arc<Mutex<Vec<(AtomicsWaitLocation, u64)>>>,
}

impl AtomicsWaiterProvider for RecordingWaiterProvider {
    fn notify(
        &mut self,
        location: AtomicsWaitLocation,
        count: u64,
    ) -> Result<u64, HostProviderError> {
        self.calls.lock().unwrap().push((location, count));
        Ok(count.min(2))
    }

    fn wait(
        &mut self,
        _location: AtomicsWaitLocation,
        timeout: Option<core::time::Duration>,
        condition: &mut dyn FnMut() -> Result<bool, HostProviderError>,
    ) -> Result<AtomicsWaitResult, HostProviderError> {
        if !condition()? {
            return Ok(AtomicsWaitResult::NotEqual);
        }
        if timeout == Some(core::time::Duration::ZERO) {
            return Ok(AtomicsWaitResult::TimedOut);
        }
        Ok(AtomicsWaitResult::Ok)
    }
}

const ATOMICS_SOURCE: &str = r#"
var shared = new SharedArrayBuffer(64);
var values = new Int32Array(shared, 8, 4);
var order = [];
var index = { valueOf() { order.push("index"); return 1; } };
var operand = { valueOf() { order.push("value"); return 7.9; } };

var stored = Atomics.store(values, index, operand);
var oldAdd = Atomics.add(values, 1, 5);
var oldSub = Atomics.sub(values, 1, 2);
var oldAnd = Atomics.and(values, 1, 6);
var oldOr = Atomics.or(values, 1, 8);
var oldXor = Atomics.xor(values, 1, 3);
var oldExchange = Atomics.exchange(values, 1, -9);
var oldCompare = Atomics.compareExchange(values, 1, -9, 42);
var loaded = Atomics.load(values, 1);

var ordinary = new Uint16Array(new ArrayBuffer(4));
var ordinaryOld = Atomics.add(ordinary, 0, 65537);

var big = new BigInt64Array(new SharedArrayBuffer(16));
var bigOld = Atomics.store(big, 0, 9n);
var bigPrevious = Atomics.add(big, 0, 4n);

var notifyOrder = [];
var notified = Atomics.notify(
  values,
  { valueOf() { notifyOrder.push("index"); return 0; } },
  { valueOf() { notifyOrder.push("count"); return 2.9; } }
);
var ordinaryBuffer = new ArrayBuffer(16, { maxByteLength: 16 });
var ordinaryWaitable = new Int32Array(ordinaryBuffer);
var ordinaryNotified = Atomics.notify(
  ordinaryWaitable,
  { valueOf() { notifyOrder.push("resize"); ordinaryBuffer.resize(0); return 0; } },
  { valueOf() { notifyOrder.push("count-after-resize"); return 1; } }
);

stored === 7 &&
oldAdd === 7 && oldSub === 12 && oldAnd === 10 && oldOr === 2 &&
oldXor === 10 && oldExchange === 9 && oldCompare === -9 && loaded === 42 &&
order.join(",") === "index,value" &&
ordinaryOld === 0 && ordinary[0] === 1 &&
bigOld === 9n && bigPrevious === 9n && big[0] === 13n &&
notified === 0 && ordinaryNotified === 0 &&
notifyOrder.join(",") === "index,count,resize,count-after-resize" &&
Atomics.isLockFree({ valueOf() { return 8; } }) === true &&
Object.prototype.toString.call(Atomics) === "[object Atomics]";
"#;

const ATOMICS_NOTIFY_STRESS_SOURCE: &str = r#"
var notifyValues = new Int32Array(new SharedArrayBuffer(16));
for (var notifyIndex = 0; notifyIndex < 256; notifyIndex++) {
  var result = Atomics.notify(
    notifyValues,
    { valueOf() { return 0; } },
    { valueOf() { return 1; } }
  );
  if (result !== 0) throw new Error("unexpected notify result");
  try {
    Atomics.notify(notifyValues, { valueOf() { throw 17; } }, 1);
    throw new Error("notify index conversion did not throw");
  } catch (error) {
    if (error !== 17) throw error;
  }
}
true;
"#;

const ATOMICS_NOTIFY_PROVIDER_SOURCE: &str = r#"
var providerValues = new Int32Array(new SharedArrayBuffer(16));
var waitOrder = [];
Atomics.notify(providerValues, 1, 1) === 1 &&
Atomics.notify(providerValues, 1) === 2 &&
Atomics.wait(providerValues, 0, 1, 1) === "not-equal" &&
Atomics.wait(providerValues, 0, 0, 0) === "timed-out" &&
Atomics.wait(providerValues, 0, 0, 1) === "ok" &&
Atomics.wait(
  providerValues,
  { valueOf() { waitOrder.push("index"); return 0; } },
  { valueOf() { waitOrder.push("expected"); return 1; } },
  { valueOf() { waitOrder.push("timeout"); return 1; } }
) === "not-equal" &&
waitOrder.join(",") === "index,expected,timeout" &&
Atomics.wait(new BigInt64Array(new SharedArrayBuffer(8)), 0, 1n, 1) === "not-equal";
"#;

#[test]
fn atomics_non_waiting_operations_support_all_dispatch_batch_sizes() {
    assert_atomics::<1>(false);
    assert_atomics::<2>(false);
    assert_atomics::<4>(false);
    assert_atomics::<8>(false);
    assert_atomics::<16>(false);
}

#[test]
fn atomics_conversion_state_survives_forced_major_collection() {
    assert_atomics::<8>(true);
}

#[test]
fn atomics_notify_releases_completed_and_abrupt_continuations() {
    let module = compile_source(ATOMICS_NOTIFY_STRESS_SOURCE, 9_300);
    let mut isolate = test_isolate_with_heap_spans(256);

    assert_completed_true::<8>(&mut isolate, &module, "Atomics.notify warmup");
    collect_major(&mut isolate);
    let baseline_completions = isolate.fiber.completions.len();
    let baseline_external = isolate.heap.external_bytes();

    assert_completed_true::<8>(&mut isolate, &module, "Atomics.notify repeat");
    collect_major(&mut isolate);
    assert_eq!(isolate.fiber.completions.len(), baseline_completions);
    assert_eq!(isolate.heap.external_bytes(), baseline_external);
}

#[test]
fn atomics_notify_routes_shared_locations_to_the_host_provider() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let provider = RecordingWaiterProvider {
        calls: Arc::clone(&calls),
    };
    let mut isolate = test_isolate_with_provider(provider);
    let module = compile_source(ATOMICS_NOTIFY_PROVIDER_SOURCE, 9_301);

    assert_completed_true::<8>(&mut isolate, &module, "Atomics.notify provider");
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, calls[1].0);
    assert_eq!(calls[0].0.byte_offset(), 4);
    assert_eq!(calls[0].1, 1);
    assert_eq!(calls[1].1, u64::MAX);
}

#[test]
fn atomics_wait_supports_all_dispatch_batch_sizes() {
    assert_atomics_wait::<1>(false);
    assert_atomics_wait::<2>(false);
    assert_atomics_wait::<4>(false);
    assert_atomics_wait::<8>(false);
    assert_atomics_wait::<16>(false);
}

#[test]
fn atomics_wait_conversion_survives_forced_major_collection() {
    assert_atomics_wait::<8>(true);
}

#[test]
fn atomics_wait_async_invalid_view_is_catchable_type_error() {
    let module = compile_source(
        r#"
var view = new Uint8Array(new SharedArrayBuffer(8));
function classify(callback) {
  try { callback(); return "none"; }
  catch (error) {
    return error instanceof TypeError ? "type" :
      error instanceof SyntaxError ? "syntax" : "other";
  }
}
classify(function() {
  Atomics.waitAsync(view, { valueOf() { throw 17; } }, 0, 0);
}) === "type";
"#,
        9_302,
    );
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 100_000,
                quantum: 100_000,
            },
        )
        .expect("invalid waitAsync view executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "{outcome:?}"
    );
}

/// Executes the shared fixture under one dispatch and collection policy.
fn assert_atomics<const N: usize>(forced_major: bool) {
    let module = compile_atomics_source(9_200 + N as u32);
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .expect("Atomics fixture executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

/// Compiles the Atomics fixture independently of runtime dispatch policy.
fn compile_atomics_source(source_id: u32) -> CompiledModule {
    compile_source(ATOMICS_SOURCE, source_id)
}

/// Compiles one Atomics fixture independently of runtime dispatch policy.
fn compile_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("atomics-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Atomics fixture compiles")
}

/// Executes a fixture and requires the boolean true completion value.
fn assert_completed_true<const N: usize>(
    isolate: &mut Isolate,
    module: &CompiledModule,
    label: &str,
) {
    let outcome = isolate
        .execute_with_batch::<N>(
            module,
            ExecutionBudget {
                fuel: 2_000_000,
                quantum: 2_000_000,
            },
        )
        .unwrap_or_else(|error| panic!("{label} executes: {error:?}"));
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "{label} must complete with true"
    );
}

/// Runs a major collection with every isolate-owned root category visible.
fn collect_major(isolate: &mut Isolate) {
    let mut roots = VmRoots {
        fiber: &mut isolate.fiber,
        suspended_fibers: &mut isolate.suspended_fibers,
        finalization_jobs: &mut isolate.finalization_jobs,
        promise_jobs: &mut isolate.promise_jobs,
        realm: &mut isolate.realm,
        inactive_realms: &mut isolate.inactive_realms,
        loaded_code: &mut isolate.loaded_code,
        module_graph: &mut isolate.module_graph,
    };
    isolate
        .heap
        .collect_major(&mut roots)
        .expect("Atomics major collection succeeds");
}

/// Executes wait conversion and provider dispatch under one VM tuning policy.
fn assert_atomics_wait<const N: usize>(forced_major: bool) {
    let provider = RecordingWaiterProvider {
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let mut isolate = test_isolate_with_provider(provider);
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let module = compile_source(ATOMICS_NOTIFY_PROVIDER_SOURCE, 9_400 + N as u32);
    assert_completed_true::<N>(&mut isolate, &module, "Atomics.wait fixture");
}

/// Builds one isolate with an injected waiter provider and the standard test limits.
fn test_isolate_with_provider(provider: impl AtomicsWaiterProvider + 'static) -> Isolate {
    Isolate::new_with_host_providers(
        IsolateConfig::new(
            AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
            HeapLimit::new(16 * SPAN_SIZE_BYTES),
            StackLimits::new(64, 4_096),
            RealmLimits::new(64, 1_024),
        ),
        HostProviders::new()
            .with_atomics_waiter(provider)
            .with_agent_can_suspend(true),
    )
    .expect("test isolate descriptors register")
}
