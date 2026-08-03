use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{
    fixtures::{test_isolate, test_isolate_with_heap_spans},
    *,
};

const ORDERING_SOURCE: &str = r#"
var trace = "";
var marker = {};
function number(label, value) {
  return { valueOf() { trace += label; return value; } };
}
var unary = Math.abs(number("a", -3));
var binary = Math.pow(number("b", 2), number("c", 3));
var maximum = Math.max(NaN, number("d", undefined));
var minimum = Math.min(number("e", 0), -0);
var later = 0;
var threw = false;
try {
  Math.hypot(
    Infinity,
    { valueOf() { trace += "f"; throw marker; } },
    { valueOf() { later++; return 1; } }
  );
} catch (error) {
  threw = error === marker;
}
var ignored = { valueOf() { trace += "x"; return 9; } };
var ignoredResult = Math.abs(-4, ignored);
var randomResult = Math.random(ignored);
unary === 3 && binary === 8 && Number.isNaN(maximum) && Object.is(minimum, -0) &&
  threw && later === 0 && ignoredResult === 4 && randomResult >= 0 && randomResult < 1 &&
  trace === "abcdef";
"#;

const GENERATOR_CONVERSION_SOURCE: &str = r#"
var filler00 = 0, filler01 = 1, filler02 = 2, filler03 = 3, filler04 = 4;
var filler05 = 5, filler06 = 6, filler07 = 7, filler08 = 8, filler09 = 9;
var filler10 = 10, filler11 = 11, filler12 = 12, filler13 = 13, filler14 = 14;
var filler15 = 15, filler16 = 16, filler17 = 17, filler18 = 18, filler19 = 19;
var ordinary = {
  valueOf: function* () { yield 1; },
  toString() { return "7"; }
};
var exotic = {
  [Symbol.toPrimitive]: function* () { yield 1; }
};
var exoticType = false;
try { Math.abs(exotic); } catch (error) { exoticType = error instanceof TypeError; }
Math.max(1, ordinary) === 7 && exoticType &&
  filler00 + filler01 + filler02 + filler03 + filler04 + filler05 + filler06 + filler07 +
  filler08 + filler09 + filler10 + filler11 + filler12 + filler13 + filler14 + filler15 +
  filler16 + filler17 + filler18 + filler19 === 190;
"#;

const LEAK_STRESS_SOURCE: &str = r#"
(function () {
  var marker = {};
  for (var index = 0; index < 512; index++) {
    if (Math.max({ valueOf() { return index; } }, 1) !== Math.max(index, 1)) return false;
    if (Math.abs({ [Symbol.toPrimitive]() { return -index; } }) !== index) return false;
    try {
      Math.hypot({ valueOf() { throw marker; } }, 1);
      return false;
    } catch (error) {
      if (error !== marker) return false;
    }
  }
  return true;
})()
"#;

const FIXTURES: [(&str, &str); 2] = [
    ("conversion ordering", ORDERING_SOURCE),
    ("generator conversion owner", GENERATOR_CONVERSION_SOURCE),
];

#[test]
fn math_object_conversion_is_stable_for_every_dispatch_batch() {
    assert_math_conversion_batch::<1>(false);
    assert_math_conversion_batch::<2>(false);
    assert_math_conversion_batch::<4>(false);
    assert_math_conversion_batch::<8>(false);
    assert_math_conversion_batch::<16>(false);
}

#[test]
fn math_object_conversion_survives_forced_major_collection() {
    assert_math_conversion_batch::<8>(true);
}

#[test]
fn completed_math_conversions_release_external_state_and_native_continuations() {
    let module = compile_source(LEAK_STRESS_SOURCE, 9_200);
    let mut isolate = test_isolate_with_heap_spans(128);

    assert_completed_true::<8>(&mut isolate, &module, "Math leak-stress warmup");
    collect_major(&mut isolate);
    let baseline_external = isolate.heap.external_bytes();
    let baseline_completions = isolate.fiber.completions.len();
    let baseline_suspended = isolate.suspended_fibers.len();

    assert_completed_true::<8>(&mut isolate, &module, "Math leak-stress repeat");
    collect_major(&mut isolate);
    assert_eq!(isolate.heap.external_bytes(), baseline_external);
    assert_eq!(isolate.fiber.completions.len(), baseline_completions);
    assert_eq!(isolate.suspended_fibers.len(), baseline_suspended);
}

/// Runs observable Math conversion and Fiber-owner fixtures under one execution policy.
fn assert_math_conversion_batch<const N: usize>(forced_major: bool) {
    for (index, (label, source)) in FIXTURES.into_iter().enumerate() {
        let module = compile_source(source, 9_000 + N as u32 * 10 + index as u32);
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
            .unwrap_or_else(|error| panic!("{label} fixture executes: {error:?}"));
        assert!(
            matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
            "{label}, dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
        );
    }
}

/// Executes one fixture and requires its final JavaScript assertion to be true.
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
        "{label} returned {outcome:?}"
    );
}

/// Runs an explicit major collection with every VM-owned root category visible to the collector.
fn collect_major(isolate: &mut Isolate) {
    let roots = &mut VmRoots {
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
        .collect_major(roots)
        .expect("Math leak-stress major collection succeeds");
}

/// Compiles one standalone Math conversion fixture.
fn compile_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("math-conversion-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Math conversion fixture compiles")
}
