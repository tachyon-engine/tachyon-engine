use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const STRING_NORMALIZATION_SOURCE: &str = r#"
var log = "";
var receiver = { toString() { log += "r"; return "e\u0301"; } };
var form = { toString() { log += "f"; return "NFC"; } };
var normalized = String.prototype.normalize.call(receiver, form);
var left = { toString() { log += "l"; return "A\u030A"; } };
var right = { toString() { log += "t"; return "\u00C5"; } };
var compared = String.prototype.localeCompare.call(left, right);

var receiverAbrupt = "";
try {
  String.prototype.localeCompare.call({
    toString() { receiverAbrupt += "r"; throw 17; }
  }, { toString() { receiverAbrupt += "bad"; return "x"; } });
} catch (error) { receiverAbrupt += error; }
var argumentAbrupt = "";
try {
  "x".normalize({ toString() { argumentAbrupt += "f"; throw 23; } });
} catch (error) { argumentAbrupt += error; }

var invalidRange = false;
try { "x".normalize("nfc"); } catch (error) { invalidRange = error instanceof RangeError; }
var symbolReceiver = false;
try { String.prototype.normalize.call(Symbol("x")); } catch (error) { symbolReceiver = error instanceof TypeError; }
var symbolThat = false;
try { "x".localeCompare(Symbol("x")); } catch (error) { symbolThat = error instanceof TypeError; }
var normalizeConstructor = false;
try { new String.prototype.normalize(); } catch (error) { normalizeConstructor = error instanceof TypeError; }
var compareConstructor = false;
try { new String.prototype.localeCompare(); } catch (error) { compareConstructor = error instanceof TypeError; }

var lone = "\u0065\u0301\uD800\u0065\u0301\uDC00";
var loneExpected = "\u00E9\uD800\u00E9\uDC00";
var compatibility = "";
for (var i = 0; i < 256; i++) compatibility += "\uFDFA";
var expanded = compatibility.normalize("NFKD");
var forms = "\u1E9B\u0323".normalize("NFC") === "\u1E9B\u0323" &&
  "\u1E9B\u0323".normalize("NFD") === "\u017F\u0323\u0307" &&
  "\u1E9B\u0323".normalize("NFKC") === "\u1E69" &&
  "\u1E9B\u0323".normalize("NFKD") === "s\u0323\u0307";

normalized === "\u00E9" && compared === 0 && log === "rflt" &&
receiverAbrupt === "r17" && argumentAbrupt === "f23" && invalidRange &&
symbolReceiver && symbolThat && normalizeConstructor && compareConstructor &&
lone.normalize() === loneExpected && forms && expanded.length > compatibility.length * 2 &&
"undefined".localeCompare() === 0 && "a".localeCompare("b") < 0 &&
"b".localeCompare("a") > 0 && "x".localeCompare("x") === 0 &&
String.prototype.normalize.name === "normalize" && String.prototype.normalize.length === 0 &&
String.prototype.localeCompare.name === "localeCompare" && String.prototype.localeCompare.length === 1;
"#;

#[test]
fn string_normalization_semantics_work_for_every_dispatch_batch() {
    assert_string_normalization::<1>(false);
    assert_string_normalization::<2>(false);
    assert_string_normalization::<4>(false);
    assert_string_normalization::<8>(false);
    assert_string_normalization::<16>(false);
}

#[test]
fn string_normalization_state_survives_forced_major_collection() {
    assert_string_normalization::<1>(true);
    assert_string_normalization::<2>(true);
    assert_string_normalization::<4>(true);
    assert_string_normalization::<8>(true);
    assert_string_normalization::<16>(true);
}

/// Executes both Unicode String methods under one dispatch and collection policy.
fn assert_string_normalization<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(10_520 + N as u32 + u32::from(forced_major) * 100),
                SourceName::new("string-normalization-fixture"),
                MediaType::JavaScript,
                Arc::from(STRING_NORMALIZATION_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("String Unicode fixture compiles");
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
        .expect("String Unicode fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
