use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const STRING_PROTOTYPE_SOURCE: &str = r#"
var trace = "";
function receiver(text) {
  return { toString() { trace += "r"; return text; } };
}
function number(value, mark) {
  return { valueOf() { trace += mark; return value; } };
}
function string(value, mark) {
  return { toString() { trace += mark; return value; } };
}

var charAt = String.prototype.charAt.call(receiver("abc"), number(1, "p"));
var charCodeAt = String.prototype.charCodeAt.call(receiver("abc"), number(2, "q"));
var at = String.prototype.at.call(receiver("abc"), number(-1, "a"));
var codePointAt = String.prototype.codePointAt.call(receiver("abc"), number(-1, "c"));
var slice = String.prototype.slice.call(receiver("abcdef"), number(1, "s"), number(4, "e"));
var substring = String.prototype.substring.call(receiver("abcdef"), number(4, "u"), number(1, "v"));
var index = String.prototype.indexOf.call(receiver("abcabc"), string("bc", "n"), number(2, "i"));
var last = String.prototype.lastIndexOf.call(receiver("abcabc"), string("bc", "m"), number(NaN, "l"));
var repeated = String.prototype.repeat.call(receiver("xy"), number(2, "t"));

var skipped = 0;
var padSame = String.prototype.padStart.call(receiver("abc"), number(2, "d"), {
  toString() { skipped++; return "!"; }
});
var padded = String.prototype.padEnd.call(receiver("a"), number(4, "g"), string("xy", "f"));
var well = String.prototype.isWellFormed.call(receiver("ok"));
var repaired = String.prototype.toWellFormed.call(receiver("ok"));

var protocol = "";
var protocolSearch = {
  get [Symbol.match]() { protocol += "m"; return false; },
  toString() { protocol += "n"; return "bc"; }
};
var contained = String.prototype.includes.call(
  { toString() { protocol += "r"; return "abc"; } },
  protocolSearch,
  { valueOf() { protocol += "p"; return 1; } }
);
var starts = String.prototype.startsWith.call("abc", "ab", 0);
var ends = String.prototype.endsWith.call("abc", "bc");
var regexpRejected = false;
try {
  String.prototype.includes.call("abc", {
    get [Symbol.match]() { protocol += "x"; return true; },
    toString() { protocol += "bad"; return "a"; }
  });
} catch (error) {
  regexpRejected = error instanceof TypeError;
}

charAt === "b" && charCodeAt === 99 && at === "c" && codePointAt === undefined &&
slice === "bcd" && substring === "bcd" && index === 4 && last === 4 &&
repeated === "xyxy" && padSame === "abc" && skipped === 0 && padded === "axyx" &&
well === true && repaired === "ok" && contained && starts && ends && regexpRejected &&
protocol === "rmnpx" &&
trace === "rprqrarcrseruvrnirmlrtrdrgfrr";
"#;

#[test]
fn generic_string_operations_resume_for_every_dispatch_batch() {
    assert_string_prototype::<1>(false);
    assert_string_prototype::<2>(false);
    assert_string_prototype::<4>(false);
    assert_string_prototype::<8>(false);
    assert_string_prototype::<16>(false);
}

#[test]
fn generic_string_operation_state_survives_forced_major_collection() {
    assert_string_prototype::<1>(true);
    assert_string_prototype::<2>(true);
    assert_string_prototype::<4>(true);
    assert_string_prototype::<8>(true);
    assert_string_prototype::<16>(true);
}

/// Executes the shared conversion-order fixture under one dispatch and collection policy.
fn assert_string_prototype<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(10_500 + N as u32 + u32::from(forced_major) * 100),
                SourceName::new("string-prototype-fixture"),
                MediaType::JavaScript,
                Arc::from(STRING_PROTOTYPE_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("generic String prototype fixture compiles");
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
        .expect("generic String prototype fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
