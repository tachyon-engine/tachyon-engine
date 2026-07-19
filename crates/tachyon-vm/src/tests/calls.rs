use super::fixtures::*;

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
