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
