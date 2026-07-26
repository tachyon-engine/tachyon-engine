use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

struct FixedClock(i64);

impl WallClockProvider for FixedClock {
    fn unix_time_milliseconds(&mut self) -> Result<i64, HostProviderError> {
        Ok(self.0)
    }
}

struct FailingClock;

impl WallClockProvider for FailingClock {
    fn unix_time_milliseconds(&mut self) -> Result<i64, HostProviderError> {
        Err(HostProviderError::Failure(17))
    }
}

struct FixedTimeZone(i64);

impl TimeZoneProvider for FixedTimeZone {
    fn offset_milliseconds_for_utc(
        &mut self,
        _utc_milliseconds: i64,
    ) -> Result<i64, HostProviderError> {
        Ok(self.0)
    }

    fn utc_milliseconds_for_local(
        &mut self,
        local_milliseconds: i64,
    ) -> Result<i64, HostProviderError> {
        Ok(local_milliseconds - self.0)
    }
}

struct FailingTimeZone;

impl TimeZoneProvider for FailingTimeZone {
    fn offset_milliseconds_for_utc(
        &mut self,
        _utc_milliseconds: i64,
    ) -> Result<i64, HostProviderError> {
        Err(HostProviderError::Failure(23))
    }

    fn utc_milliseconds_for_local(
        &mut self,
        _local_milliseconds: i64,
    ) -> Result<i64, HostProviderError> {
        Err(HostProviderError::Failure(29))
    }
}

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

const DATE_TO_JSON_SOURCE: &str = r#"
var date = new Date(0);
var iso = date.toJSON();
var order = "";
var receiver = {
  [Symbol.toPrimitive](hint) {
    order = order + "p" + hint;
    return Symbol("finite-enough");
  },
  get toISOString() {
    order = order + "g";
    return function() {
      order = order + "c" + (this === receiver) + arguments.length;
      return 42;
    };
  }
};
var generic = Date.prototype.toJSON.call(receiver);
var observedNonFinite = false;
var nonFinite = Date.prototype.toJSON.call({
  valueOf() { return -Infinity; },
  get toISOString() { observedNonFinite = true; throw 1; }
});
var boxedThis = false;
Number.prototype.toISOString = function() {
  boxedThis = typeof this === "object" && this.valueOf() === 3;
  return "boxed";
};
var boxed = Date.prototype.toJSON.call(3);
delete Number.prototype.toISOString;
Symbol.prototype.toISOString = function() { return 10; };
var symbolBoxed = Date.prototype.toJSON.call(Symbol("boxed"));
delete Symbol.prototype.toISOString;
var symbolPrototypeDescriptor = Object.getOwnPropertyDescriptor(Symbol, "prototype");
var conversionStopped = false;
try {
  Date.prototype.toJSON.call({
    [Symbol.toPrimitive]() { throw 9; },
    get toISOString() { conversionStopped = true; }
  });
} catch (error) { conversionStopped = !conversionStopped && error === 9; }
var nullThrows = false;
try { Date.prototype.toJSON.call(null); } catch (error) { nullThrows = error instanceof TypeError; }
iso === "1970-01-01T00:00:00.000Z" && generic === 42 &&
order === "pnumbergctrue0" && nonFinite === null && !observedNonFinite &&
boxed === "boxed" && boxedThis && conversionStopped && nullThrows &&
symbolBoxed === 10 &&
symbolPrototypeDescriptor.value === Symbol.prototype &&
!symbolPrototypeDescriptor.writable && !symbolPrototypeDescriptor.enumerable &&
!symbolPrototypeDescriptor.configurable &&
Date.prototype.toJSON.name === "toJSON" && Date.prototype.toJSON.length === 1;
"#;

const DATE_PARSE_SOURCE: &str = r#"
var trace = "";
var parsedObject = Date.parse({
  [Symbol.toPrimitive](hint) {
    trace = trace + hint;
    return "+275760-09-13T00:00:00.000Z";
  }
});
var descriptor = Object.getOwnPropertyDescriptor(Date, "parse");
var constructThrows = false;
try { new Date.parse(); } catch (error) { constructThrows = error instanceof TypeError; }
Date.parse("1970-01-01") === 0 &&
Date.parse("1970-01-01T01:30:00+01:30") === 0 &&
Date.parse("1969-12-31T23:59:59.9999Z") === -1 &&
Date.parse("-271821-04-20T00:00:00.000Z") === -8640000000000000 &&
parsedObject === 8640000000000000 &&
Number.isNaN(Date.parse("-271821-04-19T23:59:59.999Z")) &&
Number.isNaN(Date.parse("+275760-09-13T00:00:00.001Z")) &&
Number.isNaN(Date.parse("-000000-03-31T00:45Z")) &&
Number.isNaN(Date.parse("2023-02-29")) &&
trace === "string" && Date.parse.name === "parse" && Date.parse.length === 1 &&
descriptor.value === Date.parse && descriptor.writable &&
!descriptor.enumerable && descriptor.configurable && constructThrows;
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
fn injected_wall_clock_drives_date_now_and_zero_argument_construction() {
    assert_date_clock_batch::<1>(false);
    assert_date_clock_batch::<2>(false);
    assert_date_clock_batch::<4>(false);
    assert_date_clock_batch::<8>(false);
    assert_date_clock_batch::<16>(false);
    assert_date_clock_batch::<8>(true);
}

#[test]
fn missing_and_failing_wall_clock_providers_remain_structured_host_errors() {
    assert_eq!(
        test_isolate().date_now(),
        Err(ExecutionError::MissingWallClockProvider)
    );
    let mut isolate = date_clock_isolate(FailingClock);
    assert_eq!(
        isolate.date_now(),
        Err(ExecutionError::WallClockProvider(
            HostProviderError::Failure(17)
        ))
    );
}

#[test]
fn injected_timezone_drives_local_date_operations_for_every_dispatch_batch() {
    assert_date_timezone_batch::<1>(false);
    assert_date_timezone_batch::<2>(false);
    assert_date_timezone_batch::<4>(false);
    assert_date_timezone_batch::<8>(false);
    assert_date_timezone_batch::<16>(false);
    assert_date_timezone_batch::<8>(true);
}

#[test]
fn missing_and_failing_timezone_providers_remain_structured_host_errors() {
    let mut missing = date_clock_isolate(FixedClock(0));
    let date = missing
        .allocate_date_object(
            0.0,
            missing.realm.date_prototype.expect("Date prototype exists"),
            AllocationSpace::Young,
        )
        .expect("Date allocation succeeds");
    assert_eq!(
        missing.date_timezone_offset(date),
        Err(ExecutionError::MissingTimeZoneProvider)
    );

    let mut failing = date_host_isolate(FixedClock(0), FailingTimeZone);
    let date = failing
        .allocate_date_object(
            0.0,
            failing.realm.date_prototype.expect("Date prototype exists"),
            AllocationSpace::Young,
        )
        .expect("Date allocation succeeds");
    assert_eq!(
        failing.date_timezone_offset(date),
        Err(ExecutionError::TimeZoneProvider(
            HostProviderError::Failure(23)
        ))
    );
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
fn date_to_json_resumes_for_every_dispatch_batch() {
    assert_date_to_json_batch::<1>();
    assert_date_to_json_batch::<2>();
    assert_date_to_json_batch::<4>();
    assert_date_to_json_batch::<8>();
    assert_date_to_json_batch::<16>();
}

#[test]
fn date_to_json_state_survives_forced_major_collections() {
    let module = compile_date_program(DATE_TO_JSON_SOURCE, 1_408);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 12_288,
                quantum: 12_288,
            },
        )
        .expect("forced-major Date toJSON fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "forced-major Date toJSON fixture returned {outcome:?}"
    );
}

#[test]
fn date_parse_resumes_for_every_dispatch_batch() {
    assert_date_parse_batch::<1>();
    assert_date_parse_batch::<2>();
    assert_date_parse_batch::<4>();
    assert_date_parse_batch::<8>();
    assert_date_parse_batch::<16>();
}

#[test]
fn date_parse_conversion_survives_forced_major_collections() {
    let module = compile_date_program(DATE_PARSE_SOURCE, 1_409);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 12_288,
                quantum: 12_288,
            },
        )
        .expect("forced-major Date.parse fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "forced-major Date.parse fixture returned {outcome:?}"
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

/// Executes both clock consumers under one dispatch and forced-collection policy.
fn assert_date_clock_batch<const N: usize>(forced_major: bool) {
    let module = compile_date_program(
        r#"
var descriptor = Object.getOwnPropertyDescriptor(Date, "now");
Date.now() === 123456789 && new Date().getTime() === 123456789 &&
Date.now.name === "now" && Date.now.length === 0 &&
descriptor.value === Date.now && descriptor.writable === true &&
descriptor.enumerable === false && descriptor.configurable === true;
"#,
        1_540 + N as u32,
    );
    let mut isolate = date_clock_isolate(FixedClock(123_456_789));
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("injected Date clock fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "Date clock batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Executes both timezone directions, local mutation, and formatting under one dispatch policy.
fn assert_date_timezone_batch<const N: usize>(forced_major: bool) {
    let module = compile_date_program(
        r#"
var epoch = new Date(0);
var constructed = new Date(1970, 0, 1, 1, 30, 0, 0);
var converted = new Date({ valueOf() { return 0; } });
var parsed = new Date("1970");
var changed = new Date(0);
var setResult = changed.setHours(2, 30, 0, 0);
var called = Date("ignored");
epoch.getFullYear() === 1970 && epoch.getMonth() === 0 &&
epoch.getDate() === 1 && epoch.getDay() === 4 &&
epoch.getHours() === 1 && epoch.getMinutes() === 30 &&
epoch.getSeconds() === 0 && epoch.getMilliseconds() === 0 &&
epoch.getTimezoneOffset() === -90 && constructed.getTime() === 0 &&
converted.getTime() === 0 && parsed.getTime() === 0 &&
Date.parse("1970-01-01T01:30:00") === 0 &&
Date.parse(epoch.toString()) === 0 && Date.parse(epoch.toUTCString()) === 0 &&
setResult === 3600000 && changed.getUTCHours() === 1 &&
epoch.toString() === "Thu Jan 01 1970 01:30:00 GMT+0130" &&
epoch.toDateString() === "Thu Jan 01 1970" &&
epoch.toTimeString() === "01:30:00 GMT+0130" &&
called === "Thu Jan 01 1970 01:30:00 GMT+0130" &&
Date.prototype.getFullYear.name === "getFullYear" &&
Date.prototype.setHours.length === 4;
"#,
        1_560 + N as u32,
    );
    let mut isolate = date_host_isolate(FixedClock(0), FixedTimeZone(90 * 60 * 1_000));
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("injected timezone Date fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "Date timezone batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Creates a normal test isolate while making its wall-clock dependency explicit.
fn date_clock_isolate(provider: impl WallClockProvider + 'static) -> Isolate {
    Isolate::new_with_host_providers(
        IsolateConfig::new(
            AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
            HeapLimit::new(9 * SPAN_SIZE_BYTES),
            StackLimits::new(64, 4_096),
            RealmLimits::new(64, 1_024),
        ),
        HostProviders::new().with_wall_clock(provider),
    )
    .expect("Date provider test isolate descriptors register")
}

/// Creates a test isolate with independent wall-clock and timezone capabilities.
fn date_host_isolate(
    clock: impl WallClockProvider + 'static,
    timezone: impl TimeZoneProvider + 'static,
) -> Isolate {
    Isolate::new_with_host_providers(
        IsolateConfig::new(
            AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
            HeapLimit::new(9 * SPAN_SIZE_BYTES),
            StackLimits::new(64, 4_096),
            RealmLimits::new(64, 1_024),
        ),
        HostProviders::new()
            .with_wall_clock(clock)
            .with_time_zone(timezone),
    )
    .expect("Date host-provider test isolate descriptors register")
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

/// Executes generic Date toJSON conversion and invocation for one interpreter dispatch batch.
fn assert_date_to_json_batch<const N: usize>() {
    let module = compile_date_program(DATE_TO_JSON_SOURCE, 1_480 + N as u32);
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 12_288,
                quantum: 12_288,
            },
        )
        .expect("Date toJSON fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "Date toJSON batch {N} returned {outcome:?}"
    );
}

/// Executes Date.parse conversion and UTC/offset parsing for one interpreter dispatch batch.
fn assert_date_parse_batch<const N: usize>() {
    let module = compile_date_program(DATE_PARSE_SOURCE, 1_520 + N as u32);
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 12_288,
                quantum: 12_288,
            },
        )
        .expect("Date.parse fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "Date.parse batch {N} returned {outcome:?}"
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
