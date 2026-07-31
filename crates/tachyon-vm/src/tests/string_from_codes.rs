use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const STRING_FROM_CODES_SOURCE: &str = r#"
var log = "";
var charResult = String.fromCharCode(
  { valueOf() { log += "a"; return 65; } },
  { valueOf() { log += "b"; return {}; }, toString() { log += "c"; return "66"; } },
  -1,
  NaN,
  Infinity,
  0xD800
);
var pointResult = String.fromCodePoint(
  { valueOf() { log += "d"; return 67; } },
  0x1F600,
  0xD800
);

var stop = "";
var abruptIdentity = false;
try {
  String.fromCharCode(
    { valueOf() { stop += "x"; throw 17; } },
    { valueOf() { stop += "bad"; return 65; } }
  );
} catch (error) { abruptIdentity = error === 17; }

var bigintTypeError = false;
var bigintStopped = "";
try {
  String.fromCodePoint(1n, { valueOf() { bigintStopped += "bad"; return 65; } });
} catch (error) { bigintTypeError = error instanceof TypeError; }

var symbolTypeError = false;
try { String.fromCharCode(Symbol()); } catch (error) { symbolTypeError = error instanceof TypeError; }

var rangeCount = 0;
for (var invalid of [-1, 0x110000, 1.5, NaN, Infinity]) {
  try { String.fromCodePoint(invalid); } catch (error) {
    if (error instanceof RangeError) rangeCount++;
  }
}

charResult.charCodeAt(0) === 65 && charResult.charCodeAt(1) === 66 &&
charResult.charCodeAt(2) === 65535 && charResult.charCodeAt(3) === 0 &&
charResult.charCodeAt(4) === 0 && charResult.charCodeAt(5) === 0xD800 &&
pointResult.charCodeAt(0) === 67 && pointResult.charCodeAt(1) === 0xD83D &&
pointResult.charCodeAt(2) === 0xDE00 && pointResult.charCodeAt(3) === 0xD800 &&
log === "abcd" && abruptIdentity && stop === "x" && bigintTypeError &&
bigintStopped === "" && symbolTypeError && rangeCount === 5 &&
String.fromCharCode() === "" && String.fromCodePoint() === "";
"#;

#[test]
fn string_from_codes_semantics_work_for_every_dispatch_batch() {
    assert_string_from_codes::<1>(false);
    assert_string_from_codes::<2>(false);
    assert_string_from_codes::<4>(false);
    assert_string_from_codes::<8>(false);
    assert_string_from_codes::<16>(false);
}

#[test]
fn string_from_codes_state_survives_forced_major_collection() {
    assert_string_from_codes::<1>(true);
    assert_string_from_codes::<2>(true);
    assert_string_from_codes::<4>(true);
    assert_string_from_codes::<8>(true);
    assert_string_from_codes::<16>(true);
}

/// Executes the shared constructor fixture under one dispatch and collection policy.
fn assert_string_from_codes<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(10_640 + N as u32 + u32::from(forced_major) * 100),
                SourceName::new("string-from-codes-fixture"),
                MediaType::JavaScript,
                Arc::from(STRING_FROM_CODES_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("String code fixture compiles");
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
                fuel: 524_288,
                quantum: 524_288,
            },
        )
        .expect("String code fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
