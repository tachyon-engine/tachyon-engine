use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

#[test]
fn regexp_and_string_replace_basic_matrix() {
    let source = r#"
      var a = 'abc'.replace(/b/, 'X') === 'aXc';
      var b = 'ababa'.replace(/a/g, '<$&>') === '<a>b<a>b<a>';
      var c = 'abc'.replace(/b/, '$`-$&-$\'') === 'aa-b-cc';
      var d = RegExp.prototype[Symbol.replace].call(/x/g, 'abc', 'y') === 'abc';
      a && b && c && d && String.prototype.replace.length === 2 &&
        RegExp.prototype[Symbol.replace].length === 2;
    "#;
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(9_901),
                SourceName::new("regexp-replace-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("replace fixture compiles");
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<4>(
            &module,
            ExecutionBudget {
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .expect("replace fixture executes");
    assert!(
        matches!(
            outcome,
            RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
        ),
        "{outcome:?}"
    );
}

#[test]
fn regexp_replace_expands_positional_and_named_captures() {
    let source = r#"
      var positional = 'ab'.replace(/(a)(b)/, '$2$1') === 'ba';
      var absent = 'a'.replace(/(a)(b)?/, '<$2>') === '<>';
      var fallback = 'ab'.replace(/(a)/, '$12') === 'a2b';
      var invalid = 'ab'.replace(/(a)/, '$99') === '$99b';
      var leadingZero = 'ab'.replace(/(a)/, '$01') === 'ab';
      var named = 'ab'.replace(/(?<left>a)(?<right>b)/, '$<right>$<left>') === 'ba';
      var missingNamed = 'a'.replace(/(?<present>a)/, '$<missing>') === '';
      var literalNamed = 'a'.replace(/a/, '$<missing>') === '$<missing>';
      positional && absent && fallback && invalid && leadingZero && named &&
        missingNamed && literalNamed;
    "#;
    assert_regexp_replace_captures::<1>(source, false);
    assert_regexp_replace_captures::<2>(source, false);
    assert_regexp_replace_captures::<4>(source, false);
    assert_regexp_replace_captures::<8>(source, false);
    assert_regexp_replace_captures::<16>(source, false);
    assert_regexp_replace_captures::<4>(source, true);
}

/// Compiles and executes the capture fixture under one dispatch and collection policy.
fn assert_regexp_replace_captures<const N: usize>(source: &str, forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(9_902),
                SourceName::new("regexp-replace-captures"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("replace capture fixture compiles");
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
        .expect("replace capture fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

#[test]
fn functional_replace_resumes_for_every_dispatch_batch_and_major_gc() {
    let source = r#"
      var order = "";
      var regexp = "ab".replace(/(?<left>a)(z)?/g,
        function(match, left, absent, index, input, groups) {
          "use strict";
          order += match + left + (absent === undefined) + index + input +
            groups.left + (this === undefined);
          return { toString() { order += "T"; return "X"; } };
        });
      var plain = "abc".replace("b", function(match, index, input) {
        return match + index + input;
      });
      var emptyCalls = "";
      var empty = "ab".replace(/(?:)/g, function(match, index) {
        emptyCalls += index;
        return "-";
      });
      var unicodeGroups = "abc".replace(
        /(?<\u03c0>a)(?<$\u{104A4}>b)(?<_\u200C>c)/du,
        function(match, pi, astral, joiner, index, input, groups) {
          return groups["\u03c0"] + groups["$\u{104A4}"] + groups["_\u200C"];
        });
      regexp === "Xb" && order === "aatrue0abatrueT" &&
        plain === "ab1abcc" && empty === "-a-b-" && emptyCalls === "012" &&
        unicodeGroups === "abc";
    "#;
    assert_regexp_replace_captures::<1>(source, false);
    assert_regexp_replace_captures::<2>(source, false);
    assert_regexp_replace_captures::<4>(source, false);
    assert_regexp_replace_captures::<8>(source, false);
    assert_regexp_replace_captures::<16>(source, false);
    assert_regexp_replace_captures::<4>(source, true);
}

#[test]
fn functional_replace_preserves_many_captures_and_exception_identity() {
    let source = r#"
      var many = "abcdefghijkl".replace(
        /(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)(k)(l)/,
        function(m, a, b, c, d, e, f, g, h, i, j, k, l, index, input) {
          return l + a + index + input.length;
        });
      var thrown = {};
      var callbackIdentity = false;
      try { "x".replace(/x/, function() { throw thrown; }); }
      catch (error) { callbackIdentity = error === thrown; }
      var conversionThrown = {};
      var conversionIdentity = false;
      try {
        "x".replace(/x/, function() {
          return { toString() { throw conversionThrown; } };
        });
      } catch (error) { conversionIdentity = error === conversionThrown; }
      many === "la012" && callbackIdentity && conversionIdentity;
    "#;
    assert_regexp_replace_captures::<4>(source, false);
    assert_regexp_replace_captures::<4>(source, true);
}
