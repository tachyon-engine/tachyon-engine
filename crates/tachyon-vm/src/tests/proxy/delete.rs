use super::*;

#[test]
fn proxy_delete_resumes_for_every_dispatch_batch_and_forced_major() {
    assert_proxy_delete_batch::<1>();
    assert_proxy_delete_batch::<2>();
    assert_proxy_delete_batch::<4>();
    assert_proxy_delete_batch::<8>();
    assert_proxy_delete_batch::<16>();
}

/// Verifies the accessor trap getter, handler receiver, arguments, and result under moving GC.
fn assert_proxy_delete_batch<const N: usize>() {
    let module = proxy_delete_module();
    let mut isolate = test_isolate();
    let code = isolate.load_module(&module).unwrap();
    let getter = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(1));
    let trap = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(2));
    let target = isolate.create_ordinary_object().unwrap();
    let handler = isolate.create_ordinary_object().unwrap();
    let delete_property = isolate.intern_intrinsic_name(b"deleteProperty").unwrap();
    isolate
        .define_property(
            handler,
            delete_property.into(),
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
    let key_atom = isolate.intern_intrinsic_name(b"subject").unwrap();
    let key = isolate.atom_string_value(key_atom).unwrap();
    for (name, value) in [
        (b"proxy".as_slice(), proxy),
        (b"key".as_slice(), key),
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
    assert_eq!(
        outcome,
        RunOutcome::Completed(Value::from_immediate(Immediate::True))
    );
}

/// Builds `Reflect.deleteProperty(proxy, key)` with an accessor-backed validating trap.
fn proxy_delete_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::default();
    entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    entry.emit(Opcode::GetById, &[1, 0, 1], span).unwrap();
    entry.emit(Opcode::LoadScope, &[2, 2], span).unwrap();
    entry.emit(Opcode::LoadScope, &[3, 3], span).unwrap();
    entry.emit(Opcode::Move, &[4, 0], span).unwrap();
    entry.emit(Opcode::Move, &[5, 1], span).unwrap();
    entry.emit(Opcode::Move, &[6, 2], span).unwrap();
    entry.emit(Opcode::Move, &[7, 3], span).unwrap();
    entry
        .emit(Opcode::CallWithReceiver, &[8, 4, 2], span)
        .unwrap();
    entry.emit(Opcode::Return, &[8], span).unwrap();
    let (entry_bytecode, entry_map, entry_registers) = entry.finish().unwrap();

    let mut getter = BytecodeBuilder::default();
    getter.emit(Opcode::LoadScope, &[0, 4], span).unwrap();
    getter.emit(Opcode::Return, &[0], span).unwrap();
    let (getter_bytecode, getter_map, getter_registers) = getter.finish().unwrap();

    let mut trap = BytecodeBuilder::default();
    trap.emit(Opcode::LoadThis, &[2], span).unwrap();
    trap.emit(Opcode::LoadScope, &[3, 5], span).unwrap();
    trap.emit(Opcode::StrictEqual, &[4, 0, 3], span).unwrap();
    trap.emit(Opcode::LoadScope, &[5, 3], span).unwrap();
    trap.emit(Opcode::StrictEqual, &[6, 1, 5], span).unwrap();
    trap.emit(Opcode::LoadScope, &[7, 6], span).unwrap();
    trap.emit(Opcode::StrictEqual, &[8, 2, 7], span).unwrap();
    trap.emit(Opcode::BitwiseAnd, &[9, 4, 6], span).unwrap();
    trap.emit(Opcode::BitwiseAnd, &[10, 9, 8], span).unwrap();
    trap.emit(Opcode::Return, &[10], span).unwrap();
    let (trap_bytecode, trap_map, trap_registers) = trap.finish().unwrap();

    CompiledModule::new(
        Arc::from("proxy delete continuation"),
        Vec::new(),
        vec![
            Arc::from("Reflect"),
            Arc::from("deleteProperty"),
            Arc::from("proxy"),
            Arc::from("key"),
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
                2,
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}
