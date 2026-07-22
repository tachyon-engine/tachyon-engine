use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;

const ASSIGN_SOURCE: &str = r#"
var effects = "";
var target = {
    set x(value) { effects = effects + "s" + value; }
};
var first = {
    get x() { effects = effects + "g"; return 1; }
};
var result = Object.assign(target, first, { y: 2 }, "ab");
result === target && effects === "gs1" && result.y === 2 &&
    result[0] === "a" && result[1] === "b";
"#;

const ASSIGN_PROXY_SOURCE: &str = r#"
var effects = "";
var symbol = Symbol("key");
var source = new Proxy({ a: 1, [symbol]: 2 }, {
    ownKeys: function(target) {
        effects = effects + "o";
        return [symbol, "a"];
    },
    getOwnPropertyDescriptor: function(target, key) {
        effects = effects + "d";
        return Object.getOwnPropertyDescriptor(target, key);
    },
    get: function(target, key) {
        effects = effects + "g";
        return target[key];
    }
});
var result = Object.assign({}, source);
effects === "odgdg" && result.a === 1 && result[symbol] === 2;
"#;

#[test]
fn object_assign_continuations_are_stable_across_dispatch_batches() {
    assert_assign_batch::<1>(ASSIGN_SOURCE, 260);
    assert_assign_batch::<2>(ASSIGN_SOURCE, 261);
    assert_assign_batch::<4>(ASSIGN_SOURCE, 262);
    assert_assign_batch::<8>(ASSIGN_SOURCE, 263);
    assert_assign_batch::<16>(ASSIGN_SOURCE, 264);
}

#[test]
fn object_assign_proxy_source_is_stable_across_dispatch_batches() {
    assert_assign_batch::<1>(ASSIGN_PROXY_SOURCE, 270);
    assert_assign_batch::<2>(ASSIGN_PROXY_SOURCE, 271);
    assert_assign_batch::<4>(ASSIGN_PROXY_SOURCE, 272);
    assert_assign_batch::<8>(ASSIGN_PROXY_SOURCE, 273);
    assert_assign_batch::<16>(ASSIGN_PROXY_SOURCE, 274);
}

/// Compiles and executes the multi-source getter/setter fixture for one dispatch batch.
fn assert_assign_batch<const N: usize>(source: &str, source_id: u32) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("object-assign"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Object.assign fixture compiles");
    let mut isolate = assign_test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("Object.assign fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

fn assign_test_isolate() -> Isolate {
    Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(16 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("Object.assign test isolate initializes")
}
