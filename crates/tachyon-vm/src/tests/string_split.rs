use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const STRING_SPLIT_SOURCE: &str = r#"
var customOrder = "";
var customReceiver = {
  toString: function() {
    customOrder += "bad";
    return "unused";
  }
};
var customSeparator = {};
customSeparator[Symbol.split] = function(value, limit) {
  customOrder += "split";
  return value === customReceiver && limit === 7 ? 41 : 0;
};
var customResult = String.prototype.split.call(customReceiver, customSeparator, 7);

var conversionOrder = "";
var receiver = {
  toString: function() {
    conversionOrder += "r";
    return "a-b-c";
  }
};
var separator = {
  toString: function() {
    conversionOrder += "s";
    return "-";
  }
};
separator[Symbol.split] = undefined;
var limit = {
  valueOf: function() {
    conversionOrder += "l";
    return 2;
  }
};
var converted = String.prototype.split.call(receiver, separator, limit);

var middle = "abc".split("b");
var leading = "abc".split("a");
var trailing = "abc".split("c");
var missing = "abc".split("x");
var units = "abc".split("", 2);
var emptyEmpty = "".split("");
var emptyMissing = "".split("x");
var undefinedSeparator = "abc".split(undefined, 1);
var zeroLimit = "abc".split("-", 0);
var wrappedLimit = "a-b-c".split("-", -1);
var regexpLetters = "abc".split(/[a-z]/);
var regexpCaptures = "a1b2c".split(/(\d)/);

customResult === 41 && customOrder === "split" &&
conversionOrder === "rls" && converted.length === 2 &&
converted[0] === "a" && converted[1] === "b" &&
middle.length === 2 && middle[0] === "a" && middle[1] === "c" &&
leading.length === 2 && leading[0] === "" && leading[1] === "bc" &&
trailing.length === 2 && trailing[0] === "ab" && trailing[1] === "" &&
missing.length === 1 && missing[0] === "abc" &&
units.length === 2 && units[0] === "a" && units[1] === "b" &&
emptyEmpty.length === 0 && emptyMissing.length === 1 && emptyMissing[0] === "" &&
undefinedSeparator.length === 1 && undefinedSeparator[0] === "abc" &&
zeroLimit.length === 0 && wrappedLimit.length === 3 &&
regexpLetters.length === 4 && regexpLetters[0] === "" && regexpLetters[3] === "" &&
regexpCaptures.length === 5 && regexpCaptures[0] === "a" &&
regexpCaptures[1] === "1" && regexpCaptures[2] === "b" &&
regexpCaptures[3] === "2" && regexpCaptures[4] === "c" &&
String.prototype.split.name === "split" && String.prototype.split.length === 2;
"#;

#[test]
fn string_split_observable_semantics_work_for_every_dispatch_batch() {
    assert_string_split_source::<1>(false);
    assert_string_split_source::<2>(false);
    assert_string_split_source::<4>(false);
    assert_string_split_source::<8>(false);
    assert_string_split_source::<16>(false);
}

#[test]
fn string_split_state_survives_forced_major_collection() {
    assert_string_split_source::<8>(true);
}

#[test]
fn regexp_split_result_survives_forced_major_collection() {
    assert_single_split_source::<8>(
        "var r = 'abc'.split(/[a-z]/); r.length === 4 && r[3] === '';",
        true,
    );
    assert_single_split_source::<8>(
        "var r = 'a1b2c'.split(/(\\d)/); r.length === 5 && r[3] === '2';",
        true,
    );
}

#[test]
fn primitive_split_and_regexp_constructor_retain_strings_across_forced_major_collection() {
    assert_single_split_source::<1>(
        "String.prototype.split.call(12345, true)[0] === '12345';",
        true,
    );
    assert_single_split_source::<2>("new RegExp(12345).source === '12345';", true);
    assert_single_split_source::<4>("new RegExp('').source === '(?:)';", true);
    assert_single_split_source::<8>(
        "String.prototype.split.call(false, undefined)[0] === 'false';",
        true,
    );
    assert_single_split_source::<16>("new RegExp(false).flags === '';", true);
}

/// Executes the shared split fixture under one dispatch and collection policy.
fn assert_string_split_source<const N: usize>(forced_major: bool) {
    let module = compile_string_split_fixture();
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
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .expect("String split fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles the split fixture independently of dispatch and collection policy.
fn compile_string_split_fixture() -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_422),
                SourceName::new("string-split-fixture"),
                MediaType::JavaScript,
                Arc::from(STRING_SPLIT_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("String split fixture compiles")
}

/// Executes one focused split source under the requested collection policy.
fn assert_single_split_source<const N: usize>(source: &str, forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_423),
                SourceName::new("focused-string-split-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("focused String split fixture compiles");
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
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("focused String split fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "focused dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
