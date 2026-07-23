use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const DATE_SOURCE: &str = r#"
var positive = new Date(1.9);
var negative = new Date(-1.9);
var invalid = new Date(Infinity);
var utc = new Date(Date.UTC(2000, 1, 29, 23, 58, 57, 456));
var setters = new Date(0);
var brandThrows = false;
try { Date.prototype.getTime.call({}); } catch (error) {
  brandThrows = error instanceof TypeError;
}
positive instanceof Date &&
positive.getTime() === 1 &&
positive.valueOf() === 1 &&
negative.getTime() === -1 &&
invalid.getTime() !== invalid.getTime() &&
utc.getUTCFullYear() === 2000 && utc.getUTCMonth() === 1 &&
utc.getUTCDate() === 29 && utc.getUTCDay() === 2 &&
utc.getUTCHours() === 23 && utc.getUTCMinutes() === 58 &&
utc.getUTCSeconds() === 57 && utc.getUTCMilliseconds() === 456 &&
utc.toISOString() === "2000-02-29T23:58:57.456Z" &&
utc.toUTCString() === "Tue, 29 Feb 2000 23:58:57 GMT" &&
utc.toGMTString === utc.toUTCString &&
utc.setTime(-1) === -1 && utc.getUTCFullYear() === 1969 &&
utc.getUTCMonth() === 11 && utc.getUTCDate() === 31 &&
utc.getUTCHours() === 23 && utc.getUTCMinutes() === 59 &&
utc.getUTCSeconds() === 59 && utc.getUTCMilliseconds() === 999 &&
setters.setUTCFullYear(2000) === Date.UTC(2000, 0, 1) &&
setters.setUTCMonth(1, 29) === Date.UTC(2000, 1, 29) &&
setters.setUTCDate(1) === Date.UTC(2000, 1, 1) &&
setters.setUTCHours(23, 58, 57, 456) === Date.UTC(2000, 1, 1, 23, 58, 57, 456) &&
setters.setUTCMinutes(0, 1, 2) === Date.UTC(2000, 1, 1, 23, 0, 1, 2) &&
setters.setUTCSeconds(3, 4) === Date.UTC(2000, 1, 1, 23, 0, 3, 4) &&
setters.setUTCMilliseconds(5) === Date.UTC(2000, 1, 1, 23, 0, 3, 5) &&
invalid.setUTCFullYear(2001) === Date.UTC(2001, 0, 1) &&
Object.prototype.toString.call(positive) === "[object Date]" &&
Date.name === "Date" && Date.length === 7 &&
Date.prototype.constructor === Date && brandThrows;
"#;

const DATE_OBJECT_CONVERSION_SOURCE: &str = r#"
var log = "";
function numeric(label, value) {
  return {
    [Symbol.toPrimitive](hint) {
      log = log + label + hint;
      return value;
    }
  };
}
var utc = Date.UTC(
  numeric("y", 2000), numeric("m", 1), numeric("d", 29),
  numeric("h", 23), numeric("i", 58), numeric("s", 57), numeric("x", 456)
);
var date = new Date(0);
var setTimeResult = date.setTime(numeric("t", -1));
var setterResult = date.setUTCHours(
  numeric("H", 1), numeric("I", 2), numeric("S", 3), numeric("X", 4)
);
var invalid = new Date(NaN);
var invalidResult = invalid.setUTCMonth({
  [Symbol.toPrimitive](hint) {
    log = log + "M" + hint;
    invalid.setTime(0);
    return 2;
  }
}, numeric("D", 3));
var brandConverted = false;
try {
  Date.prototype.setTime.call({}, { valueOf() { brandConverted = true; return 1; } });
} catch (error) {}
var stopped = true;
try {
  Date.UTC({ valueOf() { throw 42; } }, { valueOf() { stopped = false; return 1; } });
} catch (error) {
  stopped = stopped && error === 42;
}
utc === 951868737456 && setTimeResult === -1 &&
setterResult === -82676996 && invalidResult !== invalidResult && invalid.getTime() === 0 &&
log === "ynumbermnumberdnumberhnumberinumbersnumberxnumber" +
       "tnumberHnumberInumberSnumberXnumberMnumberDnumber" &&
!brandConverted && stopped;
"#;

const DATE_TO_PRIMITIVE_SOURCE: &str = r#"
var method = Date.prototype[Symbol.toPrimitive];
var order = "";
var object = {
  toString() { order = order + "s"; return {}; },
  valueOf() { order = order + "v"; return 7; }
};
var defaultResult = method.call(object, "default");
var defaultOrder = order;
order = "";
var numberResult = method.call(object, "number");
var numberOrder = order;
order = "";
var stringResult = method.call({
  toString() { order = order + "S"; return "date"; },
  get valueOf() { order = order + "V"; return function() { return 1; }; }
}, "string");
var invalidHint = false;
var invalidReceiver = false;
try { method.call(object, "invalid"); } catch (error) { invalidHint = error instanceof TypeError; }
try { method.call(1, "default"); } catch (error) { invalidReceiver = error instanceof TypeError; }
var descriptor = Object.getOwnPropertyDescriptor(Date.prototype, Symbol.toPrimitive);
defaultResult === 7 && defaultOrder === "sv" &&
numberResult === 7 && numberOrder === "v" &&
stringResult === "date" && order === "S" && invalidHint && invalidReceiver &&
method.name === "[Symbol.toPrimitive]" && method.length === 1 &&
descriptor.value === method && descriptor.writable === false &&
descriptor.enumerable === false && descriptor.configurable === true;
"#;

#[test]
fn date_numeric_construction_is_stable_for_every_dispatch_batch() {
    assert_date_batch::<1>();
    assert_date_batch::<2>();
    assert_date_batch::<4>();
    assert_date_batch::<8>();
    assert_date_batch::<16>();
}

#[test]
fn date_payload_and_prototype_survive_forced_major_collections() {
    let module = compile_date_source(1_405);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("forced-major Date fixture executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

#[test]
fn date_object_numeric_arguments_resume_for_every_dispatch_batch() {
    assert_date_object_conversion_batch::<1>();
    assert_date_object_conversion_batch::<2>();
    assert_date_object_conversion_batch::<4>();
    assert_date_object_conversion_batch::<8>();
    assert_date_object_conversion_batch::<16>();
}

#[test]
fn date_object_numeric_argument_state_survives_forced_major_collections() {
    let module = compile_date_program(DATE_OBJECT_CONVERSION_SOURCE, 1_406);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("forced-major Date conversion fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "forced-major Date conversion fixture returned {outcome:?}"
    );
}

#[test]
fn date_to_primitive_resumes_for_every_dispatch_batch() {
    assert_date_to_primitive_batch::<1>();
    assert_date_to_primitive_batch::<2>();
    assert_date_to_primitive_batch::<4>();
    assert_date_to_primitive_batch::<8>();
    assert_date_to_primitive_batch::<16>();
}

#[test]
fn date_to_primitive_state_survives_forced_major_collections() {
    let module = compile_date_program(DATE_TO_PRIMITIVE_SOURCE, 1_407);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("forced-major Date toPrimitive fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "forced-major Date toPrimitive fixture returned {outcome:?}"
    );
}

#[test]
/// Exercises TimeClip conversion without relying on any host clock capability.
fn date_time_clip_covers_numeric_and_boolean_constructor_inputs() {
    for (index, (input, expected)) in [
        ("6.54321", "6"),
        ("-6.54321", "-6"),
        ("6.54321e2", "654"),
        ("-6.54321e2", "-654"),
        ("0.654321e1", "6"),
        ("-0.654321e1", "-6"),
        ("true", "1"),
        ("false", "0"),
        ("1.23e15", "1.23e15"),
        ("-1.23e15", "-1.23e15"),
        ("1.23e-15", "0"),
        ("-1.23e-15", "0"),
    ]
    .into_iter()
    .enumerate()
    {
        let source = format!("Object.is(new Date({input}).valueOf(), {expected});");
        let module = compile_date_program(&source, 1_420 + index as u32);
        let outcome = test_isolate()
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 512,
                    quantum: 512,
                },
            )
            .unwrap();
        assert!(
            matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
            "Date({input}) expected {expected}, returned {outcome:?}"
        );
    }
}

/// Compiles and executes the Date branded-object fixture for one interpreter batch size.
fn assert_date_batch<const N: usize>() {
    let module = compile_date_source(1_380 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("Date branded-object fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes observable Date numeric conversion for one interpreter dispatch batch.
fn assert_date_object_conversion_batch<const N: usize>() {
    let module = compile_date_program(DATE_OBJECT_CONVERSION_SOURCE, 1_440 + N as u32);
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("Date object conversion fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "Date object conversion batch {N} returned {outcome:?}"
    );
}

/// Executes forced ordinary Date ToPrimitive for one interpreter dispatch batch.
fn assert_date_to_primitive_batch<const N: usize>() {
    let module = compile_date_program(DATE_TO_PRIMITIVE_SOURCE, 1_460 + N as u32);
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("Date toPrimitive fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "Date toPrimitive batch {N} returned {outcome:?}"
    );
}

fn compile_date_source(source_id: u32) -> CompiledModule {
    compile_date_program(DATE_SOURCE, source_id)
}

fn compile_date_program(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("date-branded-object"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Date fixture compiles")
}
