use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const STRING_RAW_SOURCE: &str = r#"
var log = "";
var raw = new Proxy({
  0: { toString() { log += "t0"; return "a"; } },
  1: { toString() { log += "t1"; return "b"; } },
  2: { toString() { log += "t2"; return "c"; } },
  length: { valueOf() { log += "l"; return 3; } }
}, {
  get(target, key, receiver) {
    log += key === "length" ? "L" : "g" + key;
    return Reflect.get(target, key, receiver);
  }
});
var template = new Proxy({ raw: raw }, {
  get(target, key, receiver) {
    log += "R";
    return Reflect.get(target, key, receiver);
  }
});
var first = { toString() { log += "s0"; return "X"; } };
var second = { toString() { log += "s1"; return "Y"; } };
var result = String.raw(template, first, second, { toString() { log += "bad"; return "Z"; } });

var zeroGets = 0;
var zero = String.raw({ raw: new Proxy({ length: NaN }, {
  get(target, key) {
    if (key !== "length") zeroGets++;
    return target[key];
  }
}) }, { toString() { zeroGets++; return "bad"; } });

var primitiveRaw = String.raw({ raw: "ab" }, "-");
var fractional = String.raw({ raw: { 0: "q", 1: "r", length: 2.9 } }, "-");

var abrupt = "";
try {
  String.raw({ raw: {
    length: 3,
    get 0() { abrupt += "g0"; return "a"; },
    get 1() { abrupt += "bad"; return "b"; },
    get 2() { abrupt += "bad"; return "c"; }
  } }, { toString() { abrupt += "s"; throw 17; } });
} catch (error) {
  abrupt += error;
}

var missingRawThrows = false;
try { String.raw({}); } catch (error) { missingRawThrows = error instanceof TypeError; }
var notConstructor = false;
try { new String.raw({ raw: [] }); } catch (error) { notConstructor = error instanceof TypeError; }

result === "aXbYc" && log === "RLlg0t0s0g1t1s1g2t2" &&
zero === "" && zeroGets === 0 && primitiveRaw === "a-b" && fractional === "q-r" &&
abrupt === "g0s17" && missingRawThrows && notConstructor &&
String.raw.name === "raw" && String.raw.length === 1;
"#;

#[test]
fn string_raw_observable_semantics_work_for_every_dispatch_batch() {
    assert_string_raw::<1>(false);
    assert_string_raw::<2>(false);
    assert_string_raw::<4>(false);
    assert_string_raw::<8>(false);
    assert_string_raw::<16>(false);
}

#[test]
fn string_raw_state_survives_forced_major_collection() {
    assert_string_raw::<1>(true);
    assert_string_raw::<2>(true);
    assert_string_raw::<4>(true);
    assert_string_raw::<8>(true);
    assert_string_raw::<16>(true);
}

/// Executes the shared raw-template fixture under one dispatch and collection policy.
fn assert_string_raw<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(10_240 + N as u32 + u32::from(forced_major) * 100),
                SourceName::new("string-raw-fixture"),
                MediaType::JavaScript,
                Arc::from(STRING_RAW_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("String.raw fixture compiles");
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
        .expect("String.raw fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
