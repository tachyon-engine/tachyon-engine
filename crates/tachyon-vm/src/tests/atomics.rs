use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{
    fixtures::{test_isolate, test_isolate_with_heap_spans},
    *,
};

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
