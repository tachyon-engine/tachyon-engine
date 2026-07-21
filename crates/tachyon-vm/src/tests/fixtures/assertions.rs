use super::super::*;
use super::*;

/// Checks exact cursor boundaries for one arithmetic program under a selected batch size.
#[cfg(feature = "opcode-profile")]
pub(in crate::tests) fn assert_arithmetic_profile_batch<const N: usize>(
    expected_binds: u64,
    expected_flushes: u64,
) {
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &arithmetic_module(),
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    let profile = isolate.execution_profile();
    assert_eq!(profile.batch_cursor_binds(), expected_binds);
    assert_eq!(profile.batch_flushes(), expected_flushes);
    assert_eq!(profile.slow_flushes(), 1);
    assert_eq!(profile.terminal_slow_exits(), 1);
    assert_eq!(profile.fault_slow_exits(), 0);
    assert_profile_slow_exit_conservation(profile);
}

#[cfg(feature = "opcode-profile")]
pub(in crate::tests) fn assert_profile_slow_exit_conservation(profile: &ExecutionProfile) {
    let slow = profile
        .opcodes()
        .map(|(_, counts)| counts.slow)
        .sum::<u64>();
    assert_eq!(
        slow,
        profile.slow_rebinds() + profile.terminal_slow_exits() + profile.fault_slow_exits()
    );
}

pub(in crate::tests) fn assert_batch_result<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &arithmetic_module(),
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
}

pub(in crate::tests) fn assert_less_than_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &less_than_module(),
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::True)
    ));
}

pub(in crate::tests) fn assert_typeof_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &typeof_module(),
            ExecutionBudget {
                fuel: 6,
                quantum: 6,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::True)
    ));
}

pub(in crate::tests) fn assert_scope_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &scoped_var_module(),
            ExecutionBudget {
                fuel: 6,
                quantum: 6,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(7)));
}

pub(in crate::tests) fn assert_global_lexical_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &global_lexical_module(),
            ExecutionBudget {
                fuel: 5,
                quantum: 5,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

pub(in crate::tests) fn assert_function_prototype_call_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &function_prototype_call_module(),
            ExecutionBudget {
                fuel: 7,
                quantum: 7,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

/// Exercises callback-local catch, continuation resume, and GC tracing for one dispatch batch.
pub(in crate::tests) fn assert_number_continuation_batch<const N: usize>() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<N>(
            &number_continuation_module(),
            ExecutionBudget {
                fuel: 24,
                quantum: 24,
            },
        )
        .unwrap();
    assert!(
        matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ),
        "unexpected continuation outcome: {outcome:?}"
    );
}

/// Exercises a callback throw reaching the handler around the original native call site.
pub(in crate::tests) fn assert_number_continuation_throw_batch<const N: usize>() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<N>(
            &number_continuation_throw_module(),
            ExecutionBudget {
                fuel: 16,
                quantum: 16,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

/// Exercises String's string-hint callback ordering and continuation tracing for one batch.
pub(in crate::tests) fn assert_string_continuation_batch<const N: usize>() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<N>(
            &string_continuation_module(),
            ExecutionBudget {
                fuel: 20,
                quantum: 20,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::True)
    ));
}

/// Exercises numeric unary callback resume and tracing for one dispatch batch.
pub(in crate::tests) fn assert_numeric_unary_continuation_batch<const N: usize>() {
    for (opcode, expected) in [
        (Opcode::ToNumber, 7),
        (Opcode::Negate, -7),
        (Opcode::BitwiseNot, -8),
    ] {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute_with_batch::<N>(
                &numeric_unary_continuation_module(opcode, expected),
                ExecutionBudget {
                    fuel: 20,
                    quantum: 20,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ));
    }
}

/// Exercises left and right callback resume while the pending operand remains traced.
pub(in crate::tests) fn assert_primitive_binary_continuation_batch<const N: usize>() {
    for module in [
        numeric_binary_continuation_module(),
        add_continuation_module(),
        relational_continuation_module(),
    ] {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute_with_batch::<N>(
                &module,
                ExecutionBudget {
                    fuel: 40,
                    quantum: 40,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ));
    }
}

pub(in crate::tests) fn assert_bound_function_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &bound_function_call_module(),
            ExecutionBudget {
                fuel: 16,
                quantum: 16,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

pub(in crate::tests) fn assert_array_push_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &array_push_module(),
            ExecutionBudget {
                fuel: 8,
                quantum: 8,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(2)));
}

/// Creates a zero-argument bound exotic through the production allocation path.
pub(in crate::tests) fn create_test_bound_function(isolate: &mut Isolate, target: Value) -> Value {
    let undefined = Value::from_immediate(Immediate::Undefined);
    isolate.fiber.registers.resize(3, undefined);
    isolate.fiber.registers[1] = target;
    let bound = isolate
        .create_bound_function(&CallSite {
            caller_base: 0,
            destination: 0,
            callee: isolate.realm.function_prototype_bind.unwrap(),
            argument_base: 2,
            argument_source: None,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 0,
            this_value: target,
            new_target: undefined,
            construct_receiver: None,
            call_site: WordOffset::new(0),
        })
        .unwrap();
    isolate.fiber.registers[1] = bound;
    bound
}

/// Checks both nullish substitution and strict preservation in one dispatch monomorphization.
pub(in crate::tests) fn assert_this_binding_batch<const N: usize>() {
    let mut sloppy = test_isolate();
    let global_object = sloppy.realm.global_object.unwrap();
    let outcome = sloppy
        .execute_with_batch::<N>(
            &this_binding_module(FunctionStrictness::Sloppy),
            ExecutionBudget {
                fuel: 7,
                quantum: 7,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value == global_object));

    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &this_binding_module(FunctionStrictness::Strict),
            ExecutionBudget {
                fuel: 7,
                quantum: 7,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::Undefined)
    ));
}

/// Confirms dispatch-level error conversion preserves constructor identity for each batch.
pub(in crate::tests) fn assert_reference_error_batch<const N: usize>() {
    let mut isolate = test_isolate();
    let expected = isolate
        .realm
        .error_intrinsics
        .get(NativeErrorKind::Reference)
        .constructor
        .unwrap();
    let outcome = isolate
        .execute_with_batch::<N>(
            &unresolved_assignment_module(FunctionStrictness::Strict),
            ExecutionBudget {
                fuel: 2,
                quantum: 2,
            },
        )
        .unwrap();
    let RunOutcome::Thrown(error) = outcome else {
        panic!("strict unresolved assignment must throw");
    };
    assert_eq!(
        isolate.native_error_kind(error).unwrap(),
        Some(NativeErrorKind::Reference)
    );
    let constructor_atom = isolate.constructor_atom().unwrap();
    let constructor = isolate.get_data_property(error, constructor_atom).unwrap();
    assert_eq!(constructor, Some(expected));

    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &unresolved_assignment_module(FunctionStrictness::Sloppy),
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

/// Confirms the raw callable fast path retains language-level error conversion.
pub(in crate::tests) fn assert_non_callable_batch<const N: usize>() {
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &non_callable_module(),
            ExecutionBudget {
                fuel: 2,
                quantum: 2,
            },
        )
        .unwrap();
    let RunOutcome::Thrown(error) = outcome else {
        panic!("integer call must throw");
    };
    assert_eq!(
        isolate.native_error_kind(error).unwrap(),
        Some(NativeErrorKind::Type)
    );
}

/// Confirms a zero-register callee returns undefined through the caller destination.
pub(in crate::tests) fn assert_undefined_call_batch<const N: usize>() {
    let module = undefined_call_module();
    assert_eq!(
        module
            .function(FunctionId::new(1))
            .unwrap()
            .layout()
            .register_count,
        0
    );
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::Undefined)
    ));
}

pub(in crate::tests) fn assert_batch_budget<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &arithmetic_module(),
            ExecutionBudget {
                fuel: 3,
                quantum: 3,
            },
        )
        .unwrap();
    assert_eq!(outcome, RunOutcome::BudgetExhausted);
}

pub(in crate::tests) fn assert_conditional_batch<const N: usize>() {
    for (test, expected) in [(Opcode::LoadTrue, 1), (Opcode::LoadFalse, 2)] {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &conditional_module(test),
                ExecutionBudget {
                    fuel: 6,
                    quantum: 6,
                },
            )
            .unwrap();
        assert!(
            matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(expected))
        );
    }
}

pub(in crate::tests) fn assert_backedge_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &backedge_module(),
            ExecutionBudget {
                fuel: 32,
                quantum: 32,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
}

/// Checks both paths of all logical branches under one dispatch batch monomorphization.
pub(in crate::tests) fn assert_logical_batch<const N: usize>() {
    let cases = [
        (Opcode::JumpIfFalse, Opcode::LoadFalse, None, None),
        (Opcode::JumpIfFalse, Opcode::LoadTrue, None, Some(42)),
        (Opcode::JumpIfTrue, Opcode::LoadFalse, None, Some(42)),
        (Opcode::JumpIfTrue, Opcode::LoadTrue, None, None),
        (Opcode::JumpIfNotNullish, Opcode::LoadNull, None, Some(42)),
        (
            Opcode::JumpIfNotNullish,
            Opcode::LoadImmediate,
            Some(7),
            Some(7),
        ),
    ];
    for (branch, left, immediate, expected_integer) in cases {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &logical_module(branch, left, immediate),
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        let RunOutcome::Completed(value) = outcome else {
            panic!("logical module must complete");
        };
        if let Some(expected_integer) = expected_integer {
            assert_eq!(value.as_i32(), Some(expected_integer));
        } else {
            let expected = if left == Opcode::LoadFalse {
                Immediate::False
            } else {
                Immediate::True
            };
            assert_eq!(value.as_immediate(), Some(expected));
        }
    }
}

pub(in crate::tests) fn assert_switch_batch<const N: usize>() {
    for (discriminant, expected) in [(1, 10), (2, 20), (9, 20)] {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &switch_module(discriminant),
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        assert!(
            matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(expected))
        );
    }
}
