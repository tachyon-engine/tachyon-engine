use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

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
failure;
"#;

#[test]
fn regexp_match_all_works_for_every_dispatch_batch() {
    assert_match_all::<1>(false);
    assert_match_all::<2>(false);
    assert_match_all::<4>(false);
    assert_match_all::<8>(false);
    assert_match_all::<16>(false);
}

#[test]
fn regexp_match_all_iterator_survives_forced_major_collection() {
    assert_match_all::<8>(true);
}

/// Executes the same observable iterator protocol under one dispatch/collector configuration.
fn assert_match_all<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(9_950 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("regexp-match-all-fixture"),
                MediaType::JavaScript,
                Arc::from(MATCH_ALL_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("matchAll fixture compiles");
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
        .expect("matchAll fixture executes");
    assert!(
        matches!(
            outcome,
            RunOutcome::Completed(value) if value.as_i32() == Some(0)
        ),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, code={:?}",
        match outcome {
            RunOutcome::Completed(value) => value.as_i32(),
            _ => None,
        }
    );
}
