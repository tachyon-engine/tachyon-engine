use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const NUMERIC_SOURCE: &str = r#"
var descriptor = Object.getOwnPropertyDescriptor(Math, "sumPrecise");
var constructThrows = false;
try { new Math.sumPrecise([]); } catch (error) { constructThrows = error instanceof TypeError; }
var empty = Math.sumPrecise([]);
var minusZeros = Math.sumPrecise([-0, -0]);
var mixedZeros = Math.sumPrecise([-0, 0]);
var cancellation = Math.sumPrecise([1e100, 1, -1e100]);
var ordinary = Math.sumPrecise([0.1, 0.2, 0.3]);
var infinities = Math.sumPrecise([Infinity, -Infinity]);
typeof Math.sumPrecise === "function" && Math.sumPrecise.name === "sumPrecise" &&
  Math.sumPrecise.length === 1 && descriptor.writable === true &&
  descriptor.enumerable === false && descriptor.configurable === true && constructThrows &&
  Object.is(empty, -0) && Object.is(minusZeros, -0) && Object.is(mixedZeros, 0) &&
  cancellation === 1 && ordinary === 0.6 && Number.isNaN(infinities);
"#;

const ITERABLE_SOURCE: &str = r#"
var trace = "";
var array = [1, 2, 3];
array[Symbol.iterator] = function() {
  trace += "i";
  var index = 0;
  return {
    next() {
      trace += "n";
      return index < array.length ? { value: array[index++], done: false } : { done: true };
    }
  };
};
function* numbers() {
  yield 1e100;
  yield 7;
  yield -1e100;
}
function throwsType(callback) {
  try { callback(); } catch (error) { return error instanceof TypeError; }
  return false;
}
Math.sumPrecise(array) === 6 && trace === "innnn" && Math.sumPrecise(numbers()) === 7 &&
  throwsType(function() { Math.sumPrecise(); }) &&
  throwsType(function() { Math.sumPrecise(1, 2); }) &&
  throwsType(function() { Math.sumPrecise({}); });
"#;

const CLOSE_SOURCE: &str = r#"
function invalidIterator(value, returnKind) {
  var done = false;
  return {
    next() {
      if (done) return { done: true };
      done = true;
      return { value, done: false };
    },
    get return() {
      trace += "g";
      if (returnKind === "getter-throws") throw marker;
      return function() {
        trace += "r";
        if (returnKind === "call-throws") throw marker;
        return {};
      };
    }
  };
}
function iterable(iterator) {
  return { [Symbol.iterator]() { return iterator; } };
}
var marker = {};
var trace = "";
var valueOfCalled = false;
var boxed = { valueOf() { valueOfCalled = true; return 1; } };
var boxedType = false;
try { Math.sumPrecise(iterable(invalidIterator(boxed, "normal"))); }
catch (error) { boxedType = error instanceof TypeError; }
var bigintType = false;
try { Math.sumPrecise(iterable(invalidIterator(1n, "getter-throws"))); }
catch (error) { bigintType = error instanceof TypeError && error !== marker; }
var callType = false;
try { Math.sumPrecise(iterable(invalidIterator("1", "call-throws"))); }
catch (error) { callType = error instanceof TypeError && error !== marker; }
!valueOfCalled && boxedType && bigintType && callType && trace === "grggr";
"#;

const PROTOCOL_ABRUPT_SOURCE: &str = r#"
var marker = {};
var closeCount = 0;
function iterable(mode) {
  return {
    [Symbol.iterator]() {
      return {
        next() {
          if (mode === "next") throw marker;
          return {
            done: false,
            get value() { throw marker; }
          };
        },
        return() { closeCount++; return {}; }
      };
    }
  };
}
var nextCaught = false;
var valueCaught = false;
try { Math.sumPrecise(iterable("next")); } catch (error) { nextCaught = error === marker; }
try { Math.sumPrecise(iterable("value")); } catch (error) { valueCaught = error === marker; }
nextCaught && valueCaught && closeCount === 0;
"#;

const FIXTURES: [(&str, &str); 4] = [
    ("numeric and surface", NUMERIC_SOURCE),
    ("iterable and generator", ITERABLE_SOURCE),
    ("strict Number and IteratorClose", CLOSE_SOURCE),
    ("protocol abrupt", PROTOCOL_ABRUPT_SOURCE),
];

#[test]
fn math_sum_precise_is_stable_for_every_dispatch_batch() {
    assert_math_sum_precise_batch::<1>(false);
    assert_math_sum_precise_batch::<2>(false);
    assert_math_sum_precise_batch::<4>(false);
    assert_math_sum_precise_batch::<8>(false);
    assert_math_sum_precise_batch::<16>(false);
}

#[test]
fn math_sum_precise_survives_forced_major_collection() {
    assert_math_sum_precise_batch::<8>(true);
}

/// Runs every exact-sum protocol fixture under one dispatch and collection policy.
fn assert_math_sum_precise_batch<const N: usize>(forced_major: bool) {
    for (index, (label, source)) in FIXTURES.into_iter().enumerate() {
        let module = compile_source(source, 8_900 + N as u32 * 10 + index as u32);
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

/// Compiles one standalone Math fixture without coupling it to a GC policy.
fn compile_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("math-sum-precise-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Math.sumPrecise fixture compiles")
}
