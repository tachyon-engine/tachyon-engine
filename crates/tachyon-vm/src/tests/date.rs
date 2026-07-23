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
