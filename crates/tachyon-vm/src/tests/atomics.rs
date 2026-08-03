use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

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

stored === 7 &&
oldAdd === 7 && oldSub === 12 && oldAnd === 10 && oldOr === 2 &&
oldXor === 10 && oldExchange === 9 && oldCompare === -9 && loaded === 42 &&
order.join(",") === "index,value" &&
ordinaryOld === 0 && ordinary[0] === 1 &&
bigOld === 9n && bigPrevious === 9n && big[0] === 13n &&
Atomics.isLockFree({ valueOf() { return 8; } }) === true &&
Object.prototype.toString.call(Atomics) === "[object Atomics]";
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
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("atomics-fixture"),
                MediaType::JavaScript,
                Arc::from(ATOMICS_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("Atomics fixture compiles")
}
