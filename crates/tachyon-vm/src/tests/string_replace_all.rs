use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;

const REPLACE_ALL_FORCED_MINOR_SPANS: usize = 1_024;

const REPLACE_ALL_SOURCE: &str = r#"
var staticResult = "aba".replaceAll("a", "[$&][$`][$']");
var staticOk = staticResult === "[a][][ba]b[a][ab][]";
var emptyOk = "ab".replaceAll("", "-") === "-a-b-";
var literalCaptureOk = "aaa".replaceAll("a", "$1") === "$1$1$1";

var calls = "";
var functional = "aba".replaceAll("a", function(match, position, input) {
  calls += match + position + input;
  return { toString() { return position; } };
});
var functionalOk = functional === "0b2" && calls === "a0abaa2aba";

var order = "";
var receiver = { toString() { order += "t"; return "receiver"; } };
var search = {
  get [Symbol.match]() { order += "m"; return true; },
  get flags() { order += "f"; return { toString() { order += "s"; return "g"; } }; },
  get [Symbol.replace]() {
    order += "r";
    return function(original, replacement) {
      order += "c";
      return this === search && original === receiver && replacement === "value" ? "delegated" : "bad";
    };
  }
};
var delegated = String.prototype.replaceAll.call(receiver, search, "value");
var delegationOk = delegated === "delegated" && order === "mfsrc";

var rejected = false;
try { "a".replaceAll(/a/, "x"); } catch (error) { rejected = error instanceof TypeError; }

var large = "";
for (var index = 0; index < 64; index++) large += "a";
var largeCalls = 0;
var largeResult = large.replaceAll("a", function() {
  largeCalls++;
  return { toString() { return "b"; } };
});
var largeOk = largeCalls === 64 && largeResult.length === 64 && largeResult[0] === "b";

var failure = 0;
if (!staticOk) failure = 1;
else if (!emptyOk) failure = 2;
else if (!literalCaptureOk) failure = 3;
else if (!functionalOk) failure = 4;
else if (!delegationOk) failure = 5;
else if (!rejected) failure = 6;
else if (!largeOk) failure = 7;
failure;
"#;

#[test]
fn string_replace_all_works_for_every_dispatch_batch() {
    assert_string_replace_all::<1>(None);
    assert_string_replace_all::<2>(None);
    assert_string_replace_all::<4>(None);
    assert_string_replace_all::<8>(None);
    assert_string_replace_all::<16>(None);
}

#[test]
fn string_replace_all_survives_forced_collections() {
    assert_string_replace_all::<8>(Some(ForcedCollectionMode::Minor));
    assert_string_replace_all::<8>(Some(ForcedCollectionMode::Major));
}

/// Executes protocol, substitution, callback, and capacity paths under one VM policy.
fn assert_string_replace_all<const N: usize>(collection: Option<ForcedCollectionMode>) {
    let collection_id = match collection {
        None => 0,
        Some(ForcedCollectionMode::Minor) => 32,
        Some(ForcedCollectionMode::Major) => 64,
        Some(_) => 96,
    };
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(10_050 + N as u32 + collection_id),
                SourceName::new("string-replace-all-fixture"),
                MediaType::JavaScript,
                Arc::from(REPLACE_ALL_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("replaceAll fixture compiles");
    let mut isolate = replace_all_test_isolate(collection);
    if let Some(collection) = collection {
        isolate.heap.set_forced_collection_mode(collection);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 2_097_152,
                quantum: 2_097_152,
            },
        )
        .expect("replaceAll fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(0)),
        "dispatch batch {N}, collection={collection:?} returned {outcome:?}"
    );
}

/// Gives forced-minor callback churn bounded room for repeated nursery evacuation.
fn replace_all_test_isolate(collection: Option<ForcedCollectionMode>) -> Isolate {
    let spans = if collection == Some(ForcedCollectionMode::Minor) {
        REPLACE_ALL_FORCED_MINOR_SPANS
    } else {
        32
    };
    Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(spans * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("replaceAll forced-minor isolate initializes")
}
