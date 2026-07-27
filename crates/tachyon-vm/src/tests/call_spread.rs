use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;

const CALL_SPREAD_SOURCE: &str = r#"
var trace = "";
function target(a, b, c, d) {
  return this.marker === 9 && arguments.length === 4 &&
    a === 1 && b === 2 && c === 3 && d === 4;
}
var receiver = { marker: 9 };
Object.defineProperty(receiver, "method", {
  get: function() { trace += "g"; return target; }
});
var iterable = {};
iterable[Symbol.iterator] = function() {
  trace += "i";
  var index = 0;
  return {
    next: function() {
      trace += "n";
      index += 1;
      if (index > 2) return { done: true };
      return { done: false, value: index + 1 };
    }
  };
};
var result = receiver.method(1, ...iterable, ...[4]);
result && trace === "ginnn";
"#;

const CALL_SPREAD_ABRUPT_SOURCE: &str = r#"
var closed = 0;
var thrown = 0;
var iterable = {};
iterable[Symbol.iterator] = function() {
  return {
    next: function() { throw 42; },
    return: function() { closed += 1; return {}; }
  };
};
try {
  (function() {})(...iterable);
} catch (error) {
  thrown = error;
}
thrown === 42 && closed === 0;
"#;

const CALL_SPREAD_TAIL_SOURCE: &str = r#"
function loop(count) {
  "use strict";
  if (count === 0) return true;
  return loop(...[count - 1]);
}
loop(100);
"#;

#[test]
fn call_spread_preserves_order_receiver_and_multiple_spreads_for_every_batch() {
    assert_call_spread::<1>(CALL_SPREAD_SOURCE, 3_401, false);
    assert_call_spread::<2>(CALL_SPREAD_SOURCE, 3_402, false);
    assert_call_spread::<4>(CALL_SPREAD_SOURCE, 3_404, false);
    assert_call_spread::<8>(CALL_SPREAD_SOURCE, 3_408, false);
    assert_call_spread::<16>(CALL_SPREAD_SOURCE, 3_416, false);
}

#[test]
fn call_spread_iterator_abrupt_does_not_close_and_survives_forced_major() {
    assert_call_spread::<1>(CALL_SPREAD_ABRUPT_SOURCE, 3_421, false);
    assert_call_spread::<2>(CALL_SPREAD_ABRUPT_SOURCE, 3_422, false);
    assert_call_spread::<4>(CALL_SPREAD_ABRUPT_SOURCE, 3_424, false);
    assert_call_spread::<8>(CALL_SPREAD_ABRUPT_SOURCE, 3_428, false);
    assert_call_spread::<16>(CALL_SPREAD_ABRUPT_SOURCE, 3_436, false);
    assert_call_spread::<8>(CALL_SPREAD_SOURCE, 3_440, true);
    assert_call_spread::<8>(CALL_SPREAD_ABRUPT_SOURCE, 3_441, true);
}

#[test]
fn call_spread_tail_path_reuses_frames_for_every_dispatch_batch() {
    assert_call_spread::<1>(CALL_SPREAD_TAIL_SOURCE, 3_451, false);
    assert_call_spread::<2>(CALL_SPREAD_TAIL_SOURCE, 3_452, false);
    assert_call_spread::<4>(CALL_SPREAD_TAIL_SOURCE, 3_454, false);
    assert_call_spread::<8>(CALL_SPREAD_TAIL_SOURCE, 3_458, false);
    assert_call_spread::<16>(CALL_SPREAD_TAIL_SOURCE, 3_466, false);
}

/// Compiles and executes one spread-call fixture under a selected dispatch and GC policy.
fn assert_call_spread<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("call-spread-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("spread-call fixture compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(18 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("spread-call isolate initializes");
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 1_000_000,
                quantum: 1_000_000,
            },
        )
        .expect("spread-call fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
