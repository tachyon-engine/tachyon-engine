use super::{fixtures::*, *};

#[test]
fn interpreter_executes_int32_arithmetic() {
    assert_batch_result::<1>();
    assert_batch_result::<2>();
    assert_batch_result::<4>();
    assert_batch_result::<8>();
    assert_batch_result::<16>();
}

#[test]
fn int32_min_division_promotes_instead_of_overflowing_remainder() {
    let value =
        numeric_binary_hot(Opcode::Div, Value::from_i32(i32::MIN), Value::from_i32(-1)).unwrap();
    assert_eq!(value.as_f64(), Some(2_147_483_648.0));
}

#[test]
fn numeric_less_than_works_for_every_dispatch_batch() {
    assert_less_than_batch::<1>();
    assert_less_than_batch::<2>();
    assert_less_than_batch::<4>();
    assert_less_than_batch::<8>();
    assert_less_than_batch::<16>();
}

#[test]
fn typeof_and_string_equality_work_for_every_dispatch_batch() {
    assert_typeof_batch::<1>();
    assert_typeof_batch::<2>();
    assert_typeof_batch::<4>();
    assert_typeof_batch::<8>();
    assert_typeof_batch::<16>();
}

#[cfg(feature = "opcode-profile")]
#[test]
fn opcode_profile_classifies_hot_and_terminal_slow_instructions() {
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute(
            &arithmetic_module(),
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));

    let profile = isolate.execution_profile();
    assert_eq!(
        profile.opcode(Opcode::LoadImmediate),
        OpcodeExecutionCounts {
            executed: 2,
            hot: 2,
            slow: 0,
            branch_taken: 0,
            branch_not_taken: 0,
        }
    );
    assert_eq!(profile.opcode(Opcode::Add).hot, 1);
    assert_eq!(profile.opcode(Opcode::Return).slow, 1);
    assert_eq!(
        profile
            .opcodes()
            .map(|(_, counts)| counts.executed)
            .sum::<u64>(),
        4
    );
    assert_eq!(profile.batch_cursor_binds(), 1);
    assert_eq!(profile.slow_flushes(), 1);
    assert_eq!(profile.terminal_slow_exits(), 1);
    assert_eq!(profile.batch_flushes(), 0);
    assert_eq!(profile.slow_rebinds(), 0);

    isolate.reset_execution_profile();
    assert_eq!(isolate.execution_profile(), &ExecutionProfile::default());
}

#[cfg(feature = "opcode-profile")]
#[test]
fn opcode_profile_counts_every_supported_dispatch_batch_exactly() {
    assert_arithmetic_profile_batch::<1>(4, 3);
    assert_arithmetic_profile_batch::<2>(2, 1);
    assert_arithmetic_profile_batch::<4>(1, 0);
    assert_arithmetic_profile_batch::<8>(1, 0);
    assert_arithmetic_profile_batch::<16>(1, 0);
}

#[cfg(feature = "opcode-profile")]
#[test]
fn opcode_profile_separates_same_and_changed_activation_rebinds() {
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<16>(
            &undefined_call_module(),
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::Undefined)
    ));
    let profile = isolate.execution_profile();
    assert_eq!(profile.opcode(Opcode::CreateClosure).slow, 1);
    assert_eq!(profile.opcode(Opcode::Call).slow, 1);
    assert_eq!(profile.opcode(Opcode::ReturnUndefined).slow, 1);
    assert_eq!(profile.opcode(Opcode::Return).slow, 1);
    assert_eq!(profile.same_activation_rebinds(), 1);
    assert_eq!(profile.activation_rebinds(), 2);
    assert_eq!(profile.slow_rebinds(), 3);
    assert_eq!(profile.terminal_slow_exits(), 1);
    assert_eq!(profile.fault_slow_exits(), 0);
    assert_profile_slow_exit_conservation(profile);
}

#[cfg(feature = "opcode-profile")]
#[test]
fn opcode_profile_records_budget_and_conditional_branch_outcomes() {
    let mut budgeted = test_isolate();
    assert_eq!(
        budgeted
            .execute_with_batch::<8>(
                &arithmetic_module(),
                ExecutionBudget {
                    fuel: 2,
                    quantum: 2,
                },
            )
            .unwrap(),
        RunOutcome::BudgetExhausted
    );
    assert_eq!(budgeted.execution_profile().budget_flushes(), 1);
    assert_eq!(
        budgeted
            .execution_profile()
            .opcodes()
            .map(|(_, counts)| counts.executed)
            .sum::<u64>(),
        2
    );

    for (condition, taken) in [(Opcode::LoadFalse, true), (Opcode::LoadTrue, false)] {
        let mut isolate = test_isolate();
        isolate
            .execute_with_batch::<8>(
                &logical_module(Opcode::JumpIfFalse, condition, None),
                ExecutionBudget {
                    fuel: u64::MAX,
                    quantum: u32::MAX,
                },
            )
            .unwrap();
        let counts = isolate.execution_profile().opcode(Opcode::JumpIfFalse);
        assert_eq!(counts.branch_taken, u64::from(taken));
        assert_eq!(counts.branch_not_taken, u64::from(!taken));
    }

    for (contents, taken) in [(Vec::new(), true), (vec![b'x' as u16], false)] {
        let mut isolate = test_isolate();
        isolate
            .execute_with_batch::<8>(
                &heap_string_branch_module(contents),
                ExecutionBudget {
                    fuel: u64::MAX,
                    quantum: u32::MAX,
                },
            )
            .unwrap();
        let counts = isolate.execution_profile().opcode(Opcode::JumpIfFalse);
        assert_eq!(counts.slow, 1);
        assert_eq!(counts.branch_taken, u64::from(taken));
        assert_eq!(counts.branch_not_taken, u64::from(!taken));
        assert_profile_slow_exit_conservation(isolate.execution_profile());
    }
}

#[test]
/// Pins the cursor's unsafe backing invariant across owner moves and Vec reallocations.
fn bytecode_cursor_survives_owner_and_container_moves() {
    let module = arithmetic_module();
    let bytecode = module.function(module.entry_function()).unwrap().bytecode();
    // SAFETY: `owners` retains the module's Arc-backed verified function through cursor use.
    let cursor = unsafe { BytecodeCursor::new(bytecode) };
    let mut owners = Vec::with_capacity(1);
    owners.push(module);
    owners.extend((0..32).map(|_| arithmetic_module()));

    // SAFETY: zero is the verified entry instruction start for this arithmetic function.
    let instruction = unsafe { cursor.decode(WordOffset::new(0)) };
    assert_eq!(instruction.opcode, Opcode::LoadImmediate);
    assert_eq!(owners.len(), 33);
}

#[test]
/// Pins the unchecked register cursor to one prevalidated, non-reallocating activation window.
fn register_window_checks_edges_before_unchecked_access() {
    let mut registers = [
        Value::from_i32(10),
        Value::from_i32(20),
        Value::from_i32(30),
        Value::from_i32(40),
    ];
    {
        let mut window = RegisterWindow::new(&mut registers, 1, 3).unwrap();
        // SAFETY: indices zero and two are the exact first/last slots in the checked window,
        // and the fixed array cannot move or change length while `window` is used.
        unsafe {
            assert_eq!(window.read(0).as_i32(), Some(20));
            assert_eq!(window.read(2).as_i32(), Some(40));
            window.write(2, Value::from_i32(99));
        }
    }
    assert_eq!(registers[3].as_i32(), Some(99));
    assert!(RegisterWindow::new(&mut registers, 2, 3).is_none());
    assert!(RegisterWindow::new(&mut registers, 4, 0).is_some());
    assert!(RegisterWindow::new(&mut registers, 5, 0).is_none());
}

#[test]
/// Differentials every allocation-free opcode against the checked dispatcher implementation.
fn verified_hot_kernel_matches_checked_dispatch() {
    let i = Value::from_i32;
    let immediate = Value::from_immediate;
    let cases = [
        (Opcode::Nop, [0, 0, 0], [i(1), i(2), i(3)]),
        (Opcode::LoadUndefined, [0, 0, 0], [i(1), i(2), i(3)]),
        (Opcode::LoadNull, [0, 0, 0], [i(1), i(2), i(3)]),
        (Opcode::LoadFalse, [0, 0, 0], [i(1), i(2), i(3)]),
        (Opcode::LoadTrue, [0, 0, 0], [i(1), i(2), i(3)]),
        (
            Opcode::LoadImmediate,
            [0, i32::MIN as u32, 0],
            [i(1), i(2), i(3)],
        ),
        (Opcode::Move, [0, 0, 0], [i(7), i(2), i(3)]),
        (
            Opcode::Not,
            [0, 1, 0],
            [i(7), immediate(Immediate::False), i(3)],
        ),
        (Opcode::Negate, [0, 1, 0], [i(7), i(i32::MIN), i(3)]),
        (Opcode::BitwiseNot, [0, 1, 0], [i(7), i(12), i(3)]),
        (Opcode::ToNumber, [0, 1, 0], [i(7), i(-0), i(3)]),
        (Opcode::Add, [0, 0, 1], [i(i32::MAX), i(1), i(3)]),
        (Opcode::Sub, [0, 0, 1], [i(i32::MIN), i(1), i(3)]),
        (Opcode::Mul, [0, 0, 1], [i(100_000), i(100_000), i(3)]),
        (Opcode::Div, [0, 0, 1], [i(i32::MIN), i(-1), i(3)]),
        (Opcode::BitwiseAnd, [0, 0, 1], [i(0b1100), i(0b1010), i(3)]),
        (Opcode::BitwiseOr, [0, 0, 1], [i(0b1100), i(0b1010), i(3)]),
        (Opcode::BitwiseXor, [0, 0, 1], [i(0b1100), i(0b1010), i(3)]),
        (Opcode::ShiftLeft, [0, 0, 1], [i(-1), i(3), i(3)]),
        (Opcode::ShiftRight, [0, 0, 1], [i(-8), i(2), i(3)]),
        (Opcode::ShiftRightUnsigned, [0, 0, 1], [i(-1), i(1), i(3)]),
        (Opcode::Remainder, [0, 0, 1], [i(-7), i(4), i(3)]),
        (Opcode::Exponentiate, [0, 0, 1], [i(3), i(4), i(3)]),
        (Opcode::LessThan, [0, 0, 1], [i(-1), i(1), i(3)]),
        (Opcode::GreaterThan, [0, 0, 1], [i(2), i(1), i(3)]),
        (Opcode::LessEqual, [0, 0, 1], [i(1), i(1), i(3)]),
        (Opcode::GreaterEqual, [0, 0, 1], [i(1), i(1), i(3)]),
        (
            Opcode::StrictEqual,
            [0, 1, 2],
            [i(7), immediate(Immediate::Null), immediate(Immediate::Null)],
        ),
        (Opcode::Jump, [9, 0, 0], [i(1), i(2), i(3)]),
        (
            Opcode::JumpIfFalse,
            [0, 9, 0],
            [immediate(Immediate::False), i(2), i(3)],
        ),
        (
            Opcode::JumpIfTrue,
            [0, 9, 0],
            [immediate(Immediate::True), i(2), i(3)],
        ),
        (Opcode::JumpIfNotNullish, [0, 9, 0], [i(1), i(2), i(3)]),
    ];
    let module = arithmetic_module();

    for (opcode, operands, initial) in cases {
        let mut checked = test_isolate();
        let code = checked.load_module(&module).unwrap();
        checked.enter(code, module.entry_function()).unwrap();
        checked.fiber.registers.copy_from_slice(&initial);
        let fallthrough = WordOffset::new(1);
        checked.flush_cursor_pc(fallthrough);
        assert_eq!(
            checked.dispatch(code, WordOffset::new(0), opcode, operands, 0),
            Ok(None),
            "{opcode:?}"
        );
        let checked_pc = checked.fiber.frames.last().unwrap().pc;

        let mut fast_registers = initial;
        let mut window = RegisterWindow::new(&mut fast_registers, 0, 3).unwrap();
        let instruction = DecodedInstruction {
            opcode,
            width: OperandWidth::Compact,
            operands,
            operand_count: opcode.operand_count() as u8,
            word_len: 1,
        };
        let mut fast_pc = fallthrough;
        // SAFETY: the local three-slot array covers every operand in this explicit case table
        // and cannot move or resize before the hot operation returns.
        let control =
            unsafe { execute_verified_hot_instruction(&mut window, instruction, &mut fast_pc) };
        assert_eq!(control, HotControl::Continue, "{opcode:?}");
        assert_eq!(
            fast_registers,
            checked.fiber.registers.as_slice(),
            "{opcode:?}"
        );
        assert_eq!(fast_pc, checked_pc, "{opcode:?}");
    }

    let heap_value = Value::from_heap_ref(RawHeapRef::new(1).unwrap());
    let mut registers = [heap_value, heap_value, Value::from_i32(0)];
    let mut window = RegisterWindow::new(&mut registers, 0, 3).unwrap();
    for opcode in [Opcode::Not, Opcode::JumpIfFalse, Opcode::JumpIfTrue] {
        let instruction = DecodedInstruction {
            opcode,
            width: OperandWidth::Compact,
            operands: [0, 1, 0],
            operand_count: opcode.operand_count() as u8,
            word_len: 1,
        };
        // SAFETY: the fixed window covers the operands; heap-tagged truthiness must return Slow
        // before dereferencing the synthetic logical address.
        assert_eq!(
            unsafe {
                execute_verified_hot_instruction(&mut window, instruction, &mut WordOffset::new(1))
            },
            HotControl::Slow
        );
    }
}

#[test]
fn interpreter_stops_at_exact_budget_boundary() {
    assert_batch_budget::<1>();
    assert_batch_budget::<2>();
    assert_batch_budget::<4>();
    assert_batch_budget::<8>();
    assert_batch_budget::<16>();
}

#[test]
fn interpreter_rejects_zero_batch_without_panicking() {
    let result = test_isolate().execute_with_batch::<0>(
        &arithmetic_module(),
        ExecutionBudget {
            fuel: 1,
            quantum: 1,
        },
    );
    assert_eq!(
        result,
        Err(ExecutionError::InvalidDispatchBatch { batch: 0 })
    );
}

#[test]
fn public_execute_uses_the_tuned_non_scalar_dispatch_batch() {
    assert_eq!(tuning::dispatch::DEFAULT_DISPATCH_BATCH, 8);
    let outcome = test_isolate()
        .execute(
            &arithmetic_module(),
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
}

#[test]
fn max_budget_sentinel_uses_the_unbounded_loop_without_changing_results() {
    let outcome = test_isolate()
        .execute(
            &arithmetic_module(),
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
}

#[test]
fn interpreter_restarts_dispatch_after_conditional_jumps() {
    assert_conditional_batch::<1>();
    assert_conditional_batch::<2>();
    assert_conditional_batch::<4>();
    assert_conditional_batch::<8>();
    assert_conditional_batch::<16>();
}

#[test]
fn interpreter_restarts_dispatch_after_backward_jumps() {
    assert_backedge_batch::<1>();
    assert_backedge_batch::<2>();
    assert_backedge_batch::<4>();
    assert_backedge_batch::<8>();
    assert_backedge_batch::<16>();
}
