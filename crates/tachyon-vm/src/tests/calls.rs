use super::{fixtures::*, *};
use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

#[test]
fn function_prototype_call_forwards_arguments_for_every_dispatch_batch() {
    assert_function_prototype_call_batch::<1>();
    assert_function_prototype_call_batch::<2>();
    assert_function_prototype_call_batch::<4>();
    assert_function_prototype_call_batch::<8>();
    assert_function_prototype_call_batch::<16>();
}

#[test]
fn native_continuation_resumes_for_every_dispatch_batch_and_forced_major() {
    assert_number_continuation_batch::<1>();
    assert_number_continuation_batch::<2>();
    assert_number_continuation_batch::<4>();
    assert_number_continuation_batch::<8>();
    assert_number_continuation_batch::<16>();
}

#[test]
fn native_continuation_throw_reaches_original_call_site_for_every_dispatch_batch() {
    assert_number_continuation_throw_batch::<1>();
    assert_number_continuation_throw_batch::<2>();
    assert_number_continuation_throw_batch::<4>();
    assert_number_continuation_throw_batch::<8>();
    assert_number_continuation_throw_batch::<16>();
}

#[test]
fn string_hint_continuation_resumes_for_every_dispatch_batch_and_forced_major() {
    assert_string_continuation_batch::<1>();
    assert_string_continuation_batch::<2>();
    assert_string_continuation_batch::<4>();
    assert_string_continuation_batch::<8>();
    assert_string_continuation_batch::<16>();
}

#[test]
fn numeric_unary_continuations_resume_for_every_dispatch_batch_and_forced_major() {
    assert_numeric_unary_continuation_batch::<1>();
    assert_numeric_unary_continuation_batch::<2>();
    assert_numeric_unary_continuation_batch::<4>();
    assert_numeric_unary_continuation_batch::<8>();
    assert_numeric_unary_continuation_batch::<16>();
}

#[test]
fn primitive_binary_continuations_resume_for_every_dispatch_batch_and_forced_major() {
    assert_primitive_binary_continuation_batch::<1>();
    assert_primitive_binary_continuation_batch::<2>();
    assert_primitive_binary_continuation_batch::<4>();
    assert_primitive_binary_continuation_batch::<8>();
    assert_primitive_binary_continuation_batch::<16>();
}

#[test]
fn bound_argument_prefix_forwards_for_every_dispatch_batch() {
    assert_bound_function_batch::<1>();
    assert_bound_function_batch::<2>();
    assert_bound_function_batch::<4>();
    assert_bound_function_batch::<8>();
    assert_bound_function_batch::<16>();
}

#[test]
fn array_push_method_call_is_stable_for_every_dispatch_batch() {
    assert_array_push_batch::<1>();
    assert_array_push_batch::<2>();
    assert_array_push_batch::<4>();
    assert_array_push_batch::<8>();
    assert_array_push_batch::<16>();
}

#[test]
fn array_iterator_next_call_sequence_is_stable_for_every_dispatch_batch() {
    assert_array_iterator_next_batch::<1>();
    assert_array_iterator_next_batch::<2>();
    assert_array_iterator_next_batch::<4>();
    assert_array_iterator_next_batch::<8>();
    assert_array_iterator_next_batch::<16>();
}

#[test]
fn strict_and_sloppy_this_binding_work_for_every_dispatch_batch() {
    assert_this_binding_batch::<1>();
    assert_this_binding_batch::<2>();
    assert_this_binding_batch::<4>();
    assert_this_binding_batch::<8>();
    assert_this_binding_batch::<16>();
}

#[test]
fn strict_and_sloppy_unresolved_assignment_work_for_every_dispatch_batch() {
    assert_reference_error_batch::<1>();
    assert_reference_error_batch::<2>();
    assert_reference_error_batch::<4>();
    assert_reference_error_batch::<8>();
    assert_reference_error_batch::<16>();
}

#[test]
fn non_callable_values_throw_type_error_for_every_dispatch_batch() {
    assert_non_callable_batch::<1>();
    assert_non_callable_batch::<2>();
    assert_non_callable_batch::<4>();
    assert_non_callable_batch::<8>();
    assert_non_callable_batch::<16>();
}

#[test]
fn nested_bound_construct_preserves_each_new_target_substitution() {
    let mut isolate = test_isolate();
    let target = isolate.realm.array_constructor.unwrap();
    let first = create_test_bound_function(&mut isolate, target);
    let second = create_test_bound_function(&mut isolate, first);

    let (resolved, new_target) = isolate
        .resolve_bound_construct_target(second, first)
        .unwrap();
    assert_eq!((resolved, new_target), (target, target));
    let (resolved, new_target) = isolate
        .resolve_bound_construct_target(second, second)
        .unwrap();
    assert_eq!((resolved, new_target), (target, target));
}

/// Exercises raw native helpers before the managed-error dispatch boundary.
#[test]
fn explicit_function_frames_work_for_every_dispatch_batch() {
    assert_call_batch::<1>();
    assert_call_batch::<2>();
    assert_call_batch::<4>();
    assert_call_batch::<8>();
    assert_call_batch::<16>();
}

#[test]
fn zero_register_undefined_returns_work_for_every_dispatch_batch() {
    assert_undefined_call_batch::<1>();
    assert_undefined_call_batch::<2>();
    assert_undefined_call_batch::<4>();
    assert_undefined_call_batch::<8>();
    assert_undefined_call_batch::<16>();
}

#[test]
fn captured_environments_work_for_every_dispatch_batch() {
    assert_captured_environment_batch::<1>();
    assert_captured_environment_batch::<2>();
    assert_captured_environment_batch::<4>();
    assert_captured_environment_batch::<8>();
    assert_captured_environment_batch::<16>();
}

#[test]
fn callee_throw_exits_every_dispatch_batch_without_native_unwind() {
    assert_throw_batch::<1>();
    assert_throw_batch::<2>();
    assert_throw_batch::<4>();
    assert_throw_batch::<8>();
    assert_throw_batch::<16>();
}

#[test]
fn completion_records_preserve_return_and_throw_for_every_dispatch_batch() {
    assert_call_batch::<1>();
    assert_call_batch::<2>();
    assert_call_batch::<4>();
    assert_call_batch::<8>();
    assert_call_batch::<16>();
    assert_throw_batch::<1>();
    assert_throw_batch::<2>();
    assert_throw_batch::<4>();
    assert_throw_batch::<8>();
    assert_throw_batch::<16>();
}

#[test]
fn cross_module_call_switches_code_for_every_dispatch_batch() {
    assert_cross_code_batch::<1>();
    assert_cross_code_batch::<2>();
    assert_cross_code_batch::<4>();
    assert_cross_code_batch::<8>();
    assert_cross_code_batch::<16>();
}

#[test]
fn method_calls_preserve_receiver_for_every_dispatch_batch() {
    assert_method_receiver_batch::<1>();
    assert_method_receiver_batch::<2>();
    assert_method_receiver_batch::<4>();
    assert_method_receiver_batch::<8>();
    assert_method_receiver_batch::<16>();
}

#[test]
fn construct_receiver_and_primitive_return_work_for_every_dispatch_batch() {
    assert_construct_batch::<1>();
    assert_construct_batch::<2>();
    assert_construct_batch::<4>();
    assert_construct_batch::<8>();
    assert_construct_batch::<16>();
}

#[test]
fn instanceof_walks_prototypes_for_every_dispatch_batch() {
    assert_instanceof_batch::<1>();
    assert_instanceof_batch::<2>();
    assert_instanceof_batch::<4>();
    assert_instanceof_batch::<8>();
    assert_instanceof_batch::<16>();
}

#[test]
fn strict_tail_calls_reuse_frames_for_every_dispatch_batch() {
    assert_tail_call_batch::<1>();
    assert_tail_call_batch::<2>();
    assert_tail_call_batch::<4>();
    assert_tail_call_batch::<8>();
    assert_tail_call_batch::<16>();
}

#[test]
fn strict_tail_calls_from_finally_discard_saved_completions() {
    let source = r#"
        function loop(n) {
            "use strict";
            if (n === 0) return true;
            try {} finally { return loop(n - 1); }
        }
        loop(100000);
    "#;
    assert_tail_source::<8>(source, 2);
}

#[test]
fn tail_call_fallbacks_preserve_handlers_native_calls_and_arguments() {
    let source = r#"
        function fail() { throw 7; }
        function guarded() {
            "use strict";
            try { return fail(); } catch (error) { return error === 7; }
        }
        function read(value) {
            "use strict";
            value = 9;
            return arguments[0] === 1;
        }
        function nativeTail() { "use strict"; return Number("3"); }
        guarded() && read(1) && nativeTail() === 3;
    "#;
    assert_tail_source::<8>(source, 3);
}

#[test]
fn named_tail_target_survives_forced_major_environment_replacement() {
    let source = r#"
        (function loop(n) {
            "use strict";
            if (n === 0) return true;
            return loop(n - 1);
        }(4));
    "#;
    let module = compile_tail_source(source, 4);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("forced-major tail-call fixture executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

/// Exercises direct, conditional, logical, comma, and receiver tail forms past frame limits.
fn assert_tail_call_batch<const N: usize>() {
    let source = r#"
        function direct(n, acc) {
            "use strict";
            if (n === 0) return acc;
            return direct(n - 1, acc + 1);
        }
        function forms(n) {
            "use strict";
            if (n === 0) return true;
            return n === 1 ? forms(0) : (0, n && forms(n - 1));
        }
        class Counter {
            run(n) {
                if (n === 0) return true;
                return this.run(n - 1);
            }
        }
        direct(100000, 0) === 100000 && forms(100000) && new Counter().run(100000);
    "#;
    assert_tail_source::<N>(source, 10 + N as u32);
}

/// Compiles and executes one tail-call source with enough fuel but the default shallow frame cap.
fn assert_tail_source<const N: usize>(source: &str, source_id: u32) {
    let module = compile_tail_source(source, source_id);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 10_000_000,
                quantum: 10_000_000,
            },
        )
        .expect("tail-call fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

fn compile_tail_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("tail-call"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("tail-call fixture compiles")
}
