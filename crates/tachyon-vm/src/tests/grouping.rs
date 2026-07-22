use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::*;
use crate::tests::fixtures::test_isolate;

const GROUP_BY_SOURCE: &str = r#"
var groups = Object.groupBy([1, 2, 3], function(value) {
    return value % 2 === 0 ? "even" : "odd";
});
Object.getPrototypeOf(groups) === null &&
    groups.even.length === 1 && groups.even[0] === 2 &&
    groups.odd.length === 2 && groups.odd[0] === 1 && groups.odd[1] === 3;
"#;

const GROUP_BY_CLOSE_SOURCE: &str = r#"
var closed = false;
var iterable = {
    [Symbol.iterator]: function() {
        return {
            next: function() { return { done: false, value: 1 }; },
            return: function() { closed = true; return {}; }
        };
    }
};
try {
    Object.groupBy(iterable, function() { throw 1; });
} catch (error) {}
closed;
"#;

#[test]
fn object_group_by_is_stable_across_dispatch_batches() {
    assert_group_by_batch::<1>(GROUP_BY_SOURCE, 240);
    assert_group_by_batch::<2>(GROUP_BY_SOURCE, 241);
    assert_group_by_batch::<4>(GROUP_BY_SOURCE, 242);
    assert_group_by_batch::<8>(GROUP_BY_SOURCE, 243);
    assert_group_by_batch::<16>(GROUP_BY_SOURCE, 244);
}

#[test]
fn object_group_by_closes_iterators_across_dispatch_batches() {
    assert_group_by_batch::<1>(GROUP_BY_CLOSE_SOURCE, 250);
    assert_group_by_batch::<2>(GROUP_BY_CLOSE_SOURCE, 251);
    assert_group_by_batch::<4>(GROUP_BY_CLOSE_SOURCE, 252);
    assert_group_by_batch::<8>(GROUP_BY_CLOSE_SOURCE, 253);
    assert_group_by_batch::<16>(GROUP_BY_CLOSE_SOURCE, 254);
}

/// Compiles and executes one grouping fixture with the selected dispatch batch.
fn assert_group_by_batch<const N: usize>(source: &str, source_id: u32) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("object-group-by"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Object.groupBy fixture compiles");
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("Object.groupBy fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}
