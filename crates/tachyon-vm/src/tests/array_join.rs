use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_JOIN_SOURCE: &str = r#"
var trace = "";
var source = {
  get length() {
    trace += "l;";
    return { valueOf: function() { trace += "L;"; return 3; } };
  },
  get 0() {
    trace += "g0;";
    return { toString: function() { trace += "v0;"; return "a"; } };
  },
  get 1() { trace += "g1;"; return null; },
  get 2() {
    trace += "g2;";
    return { toString: function() { trace += "v2;"; return "c"; } };
  }
};
var separator = {
  toString: function() { trace += "s;"; return "::"; }
};
var joined = Array.prototype.join.call(source, separator);
var ordered = joined === "a::::c" && trace === "l;L;s;g0;v0;g1;g2;v2;";

var target = { 0: "x", 2: "z", length: 3 };
var proxyTrace = "";
var proxy = new Proxy(target, {
  get: function(object, key, receiver) {
    proxyTrace += "g" + key + ";";
    return Reflect.get(object, key, receiver);
  }
});
var proxied = Array.prototype.join.call(proxy, "-") === "x--z" &&
  proxyTrace === "glength;g0;g1;g2;";

var self = [];
self[0] = self;
self[1] = 1;
var cycle = self.join() === ",1";

ordered && proxied && cycle && Array.prototype.join.call(true) === "";
"#;

const ARRAY_JOIN_GC_SOURCE: &str = r#"
var retained = { name: "kept" };
var source = {
  length: 2,
  get 0() { return retained; },
  get 1() { return { toString: function() { return retained.name; } }; }
};
retained.toString = function() { return this.name; };
Array.prototype.join.call(source, { toString: function() { return "/"; } }) ===
  "kept/kept";
"#;

const ARRAY_JOIN_LONG_SOURCE: &str = r#"
var values = [];
var index = 0;
while (index < 3000) {
  values.push("x");
  index += 1;
}
var joined = values.join("-");
joined.length === 5999 && joined[0] === "x" && joined[2999] === "-" &&
  joined[5998] === "x";
"#;

const ARRAY_LOCALE_PRIMITIVE_SOURCE: &str = r#"
Boolean.prototype.toLocaleString = function() { return "x"; };
[true, false].toLocaleString() === "x,x";
"#;

#[test]
fn array_join_is_stable_for_every_dispatch_batch() {
    assert_array_join_source::<1>(ARRAY_JOIN_SOURCE, 2_101, false);
    assert_array_join_source::<2>(ARRAY_JOIN_SOURCE, 2_102, false);
    assert_array_join_source::<4>(ARRAY_JOIN_SOURCE, 2_104, false);
    assert_array_join_source::<8>(ARRAY_JOIN_SOURCE, 2_108, false);
    assert_array_join_source::<16>(ARRAY_JOIN_SOURCE, 2_116, false);
}

#[test]
fn array_join_state_survives_forced_major_collections() {
    assert_array_join_source::<8>(ARRAY_JOIN_SOURCE, 2_120, true);
    assert_array_join_source::<8>(ARRAY_JOIN_GC_SOURCE, 2_121, true);
}

#[test]
fn array_to_locale_string_boxes_lookup_but_preserves_primitive_call_receiver() {
    assert_array_join_source::<1>(ARRAY_LOCALE_PRIMITIVE_SOURCE, 2_123, false);
    assert_array_join_source::<4>(ARRAY_LOCALE_PRIMITIVE_SOURCE, 2_124, true);
}

#[test]
/// Uses larger quotas because 3,000 generic indexed Gets materialize atoms and shapes.
fn array_join_growth_does_not_recurse_or_mutate_published_backing() {
    let module = compile_array_join_source(ARRAY_JOIN_LONG_SOURCE, 2_122);
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(8_192, 2 * 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(24 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 8_192).with_max_shapes(8_192),
    ))
    .expect("large-capacity join isolate initializes");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("long synchronous join executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "long synchronous join returned {outcome:?}"
    );
}

/// Compiles and executes one join fixture under a dispatch and GC policy.
fn assert_array_join_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = compile_array_join_source(source, source_id);
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
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("join fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles one join fixture independently of isolate policy.
fn compile_array_join_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-join-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("join fixture compiles")
}
