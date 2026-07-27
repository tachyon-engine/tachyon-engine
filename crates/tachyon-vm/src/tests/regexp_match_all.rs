use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const MATCH_ALL_FORCED_MINOR_SPANS: usize = 1_024;

const MATCH_ALL_SOURCE: &str = r#"
var rejected = false;
try { "aba".matchAll(/a/); } catch (error) { rejected = error instanceof TypeError; }

var regexp = /a/g;
regexp.lastIndex = 1;
var iterator = "ba".matchAll(regexp);
var first = iterator.next();
var finished = iterator.next();
var clonedCursor = first.value[0] === "a" && first.value.index === 1 &&
  !first.done && finished.done && regexp.lastIndex === 1;
var identity = iterator[Symbol.iterator]() === iterator;

var unicode = "\ud83d\ude00".matchAll(/(?:)/gu);
var zero = unicode.next();
var two = unicode.next();
var unicodeDone = unicode.next();
var unicodeAdvance = zero.value.index === 0 && two.value.index === 2 && unicodeDone.done;

var direct = RegExp.prototype[Symbol.matchAll].call(/b/g, "abc").next();
var directOk = direct.value[0] === "b" && direct.value.index === 1;

var originalExec = RegExp.prototype.exec;
var customResult = { get 0() { return { toString() { return ""; } }; } };
var customMatcher;
var customCalls = 0;
var customIterator = /./g[Symbol.matchAll]("ab");
RegExp.prototype.exec = function(input) {
  customMatcher = this;
  customCalls++;
  return customResult;
};
var customFirst = customIterator.next();
var customSecond = customIterator.next();
var customOk = customFirst.value === customResult && !customFirst.done &&
  customSecond.value === customResult && !customSecond.done &&
  customCalls === 2 && customMatcher.lastIndex === 2;

var marker = {};
var getterThrowOk = false;
var getterIterator = /./g[Symbol.matchAll]("");
Object.defineProperty(RegExp.prototype, "exec", {
  configurable: true,
  get() { throw marker; }
});
try { getterIterator.next(); } catch (error) { getterThrowOk = error === marker; }

var callThrowOk = false;
Object.defineProperty(RegExp.prototype, "exec", {
  configurable: true,
  writable: true,
  value() { throw marker; }
});
var callIterator = /./g[Symbol.matchAll]("");
try { callIterator.next(); } catch (error) { callThrowOk = error === marker; }
Object.defineProperty(RegExp.prototype, "exec", {
  configurable: true,
  writable: true,
  value: originalExec
});

var failure = 0;
if (!rejected) failure = 1;
else if (!clonedCursor) failure = 2;
else if (!identity) failure = 3;
else if (zero.value.index !== 0) failure = 41;
else if (two.value.index !== 2) failure = 100 + two.value.index;
else if (!unicodeDone.done) failure = 43;
else if (!directOk) failure = 5;
else if (String.prototype.matchAll.length !== 1) failure = 6;
else if (RegExp.prototype[Symbol.matchAll].length !== 1) failure = 7;
else if (!customOk) failure = 8;
else if (!getterThrowOk) failure = 9;
else if (!callThrowOk) failure = 10;
failure;
"#;

#[test]
fn regexp_match_all_works_for_every_dispatch_batch() {
    assert_match_all::<1>(None);
    assert_match_all::<2>(None);
    assert_match_all::<4>(None);
    assert_match_all::<8>(None);
    assert_match_all::<16>(None);
}

#[test]
fn regexp_match_all_iterator_survives_forced_collections() {
    assert_match_all::<8>(Some(ForcedCollectionMode::Minor));
    assert_match_all::<8>(Some(ForcedCollectionMode::Major));
}

/// Executes the same observable iterator protocol under one dispatch/collector configuration.
fn assert_match_all<const N: usize>(collection: Option<ForcedCollectionMode>) {
    let collection_id = match collection {
        None => 0,
        Some(ForcedCollectionMode::Minor) => 32,
        Some(ForcedCollectionMode::Major) => 64,
        Some(_) => 96,
    };
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(9_950 + N as u32 + collection_id),
                SourceName::new("regexp-match-all-fixture"),
                MediaType::JavaScript,
                Arc::from(MATCH_ALL_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("matchAll fixture compiles");
    let mut isolate = match_all_test_isolate(collection);
    if let Some(collection) = collection {
        isolate.heap.set_forced_collection_mode(collection);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 524_288,
                quantum: 524_288,
            },
        )
        .expect("matchAll fixture executes");
    assert!(
        matches!(
            outcome,
            RunOutcome::Completed(value) if value.as_i32() == Some(0)
        ),
        "dispatch batch {N}, collection={collection:?} returned {outcome:?}, code={:?}",
        match outcome {
            RunOutcome::Completed(value) => value.as_i32(),
            _ => None,
        }
    );
}

/// Gives forced-minor iterator callbacks room for repeated nursery evacuation.
fn match_all_test_isolate(collection: Option<ForcedCollectionMode>) -> Isolate {
    if collection != Some(ForcedCollectionMode::Minor) {
        return test_isolate();
    }
    Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(MATCH_ALL_FORCED_MINOR_SPANS * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("RegExp matchAll forced-minor isolate initializes")
}
