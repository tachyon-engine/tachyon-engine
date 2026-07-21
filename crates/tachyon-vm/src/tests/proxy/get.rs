use super::*;

#[test]
fn proxy_get_resumes_for_every_dispatch_batch_and_forced_major() {
    assert_proxy_get_batch::<1>();
    assert_proxy_get_batch::<2>();
    assert_proxy_get_batch::<4>();
    assert_proxy_get_batch::<8>();
    assert_proxy_get_batch::<16>();
}

fn assert_proxy_get_batch<const N: usize>() {
    let module = proxy_get_module();
    let mut isolate = test_isolate();
    let code = isolate.load_module(&module).unwrap();
    let getter = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(1));
    let trap = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(2));
    let target = isolate.create_ordinary_object().unwrap();
    let handler = isolate.create_ordinary_object().unwrap();
    let receiver = isolate.create_ordinary_object().unwrap();
    let get = isolate.intern_intrinsic_name(b"get").unwrap();
    isolate
        .define_property(
            handler,
            get.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(getter),
                setter: None,
                enumerable: Some(true),
                configurable: Some(true),
            }),
        )
        .unwrap();
    isolate.fiber.registers = vec![target, handler];
    let proxy = isolate
        .create_proxy_from_site(&proxy_call_site(&isolate, 2))
        .unwrap();
    let key = isolate.atom_string_value(get).unwrap();
    for (name, value) in [
        (b"proxy".as_slice(), proxy),
        (b"key".as_slice(), key),
        (b"receiver".as_slice(), receiver),
        (b"trap".as_slice(), trap),
        (b"expectedTarget".as_slice(), target),
        (b"expectedHandler".as_slice(), handler),
    ] {
        let atom = isolate.intern_intrinsic_name(name).unwrap();
        isolate.realm.set(atom, value).unwrap();
    }
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 128,
                quantum: 128,
            },
        )
        .unwrap();
    assert!(
        matches!(
            outcome,
            RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
        ),
        "unexpected outcome: {outcome:?}, immediate: {:?}",
        match outcome {
            RunOutcome::Completed(value) | RunOutcome::Thrown(value) => value.as_immediate(),
            RunOutcome::BudgetExhausted => None,
        }
    );
}

fn proxy_get_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::default();
    entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    entry.emit(Opcode::GetById, &[1, 0, 1], span).unwrap();
    entry.emit(Opcode::LoadScope, &[2, 2], span).unwrap();
    entry.emit(Opcode::LoadScope, &[3, 3], span).unwrap();
    entry.emit(Opcode::LoadScope, &[4, 4], span).unwrap();
    entry.emit(Opcode::Move, &[5, 0], span).unwrap();
    entry.emit(Opcode::Move, &[6, 1], span).unwrap();
    entry.emit(Opcode::Move, &[7, 2], span).unwrap();
    entry.emit(Opcode::Move, &[8, 3], span).unwrap();
    entry.emit(Opcode::Move, &[9, 4], span).unwrap();
    entry
        .emit(Opcode::CallWithReceiver, &[10, 5, 3], span)
        .unwrap();
    entry.emit(Opcode::StrictEqual, &[11, 10, 4], span).unwrap();
    entry.emit(Opcode::Return, &[11], span).unwrap();
    let (entry_bytecode, entry_map, entry_registers) = entry.finish().unwrap();

    let mut getter = BytecodeBuilder::default();
    getter.emit(Opcode::LoadScope, &[0, 5], span).unwrap();
    getter.emit(Opcode::Return, &[0], span).unwrap();
    let (getter_bytecode, getter_map, getter_registers) = getter.finish().unwrap();

    let mut trap = BytecodeBuilder::default();
    trap.emit(Opcode::LoadThis, &[3], span).unwrap();
    trap.emit(Opcode::LoadScope, &[4, 6], span).unwrap();
    trap.emit(Opcode::StrictEqual, &[5, 0, 4], span).unwrap();
    trap.emit(Opcode::LoadScope, &[6, 3], span).unwrap();
    trap.emit(Opcode::StrictEqual, &[7, 1, 6], span).unwrap();
    trap.emit(Opcode::LoadScope, &[8, 4], span).unwrap();
    trap.emit(Opcode::StrictEqual, &[9, 2, 8], span).unwrap();
    trap.emit(Opcode::LoadScope, &[10, 7], span).unwrap();
    trap.emit(Opcode::StrictEqual, &[11, 3, 10], span).unwrap();
    trap.emit(Opcode::BitwiseAnd, &[12, 5, 7], span).unwrap();
    trap.emit(Opcode::BitwiseAnd, &[13, 12, 9], span).unwrap();
    trap.emit(Opcode::BitwiseAnd, &[14, 13, 11], span).unwrap();
    let invalid = trap.new_label().unwrap();
    trap.emit_jump_if_false(RegisterId::new(14), invalid, span)
        .unwrap();
    trap.emit(Opcode::Return, &[2], span).unwrap();
    trap.bind_label(invalid).unwrap();
    trap.emit(Opcode::LoadFalse, &[15], span).unwrap();
    trap.emit(Opcode::Return, &[15], span).unwrap();
    let (trap_bytecode, trap_map, trap_registers) = trap.finish().unwrap();

    CompiledModule::new(
        Arc::from("proxy get continuation"),
        Vec::new(),
        vec![
            Arc::from("Reflect"),
            Arc::from("get"),
            Arc::from("proxy"),
            Arc::from("key"),
            Arc::from("receiver"),
            Arc::from("trap"),
            Arc::from("expectedTarget"),
            Arc::from("expectedHandler"),
        ],
        vec![
            proxy_test_template(
                FunctionId::new(0),
                entry_bytecode,
                entry_map,
                entry_registers,
                0,
            ),
            proxy_test_template(
                FunctionId::new(1),
                getter_bytecode,
                getter_map,
                getter_registers,
                0,
            ),
            proxy_test_template(
                FunctionId::new(2),
                trap_bytecode,
                trap_map,
                trap_registers,
                3,
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}
