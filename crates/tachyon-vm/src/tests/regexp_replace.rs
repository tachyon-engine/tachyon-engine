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
