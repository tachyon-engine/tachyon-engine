use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;

const OWN_KEYS_LINEAR_SOURCE: &str = r#"
var keys = [];
for (var i = 0; i < 4096; i++) keys.push("key" + i);
var proxy = new Proxy({}, { ownKeys: function() { return keys; } });
Object.getOwnPropertyNames(proxy).length === 4096;
"#;

#[test]
fn proxy_own_keys_synchronous_elements_do_not_grow_the_rust_stack() {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(9_101),
                SourceName::new("proxy-own-keys-linear"),
                MediaType::JavaScript,
                Arc::from(OWN_KEYS_LINEAR_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("Proxy ownKeys stack fixture compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(16_384, 8 * 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(64 * SPAN_SIZE_BYTES),
        StackLimits::new(128, 32_768),
        RealmLimits::new(16_384, 32_768),
    ))
    .expect("Proxy ownKeys stack isolate initializes");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 4_000_000,
                quantum: 4_000_000,
            },
        )
        .expect("Proxy ownKeys stack fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "unexpected Proxy ownKeys outcome: {outcome:?}"
    );
}
