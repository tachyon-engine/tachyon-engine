use super::*;

#[test]
fn proxy_define_resumes_descriptor_and_trap_getters_for_every_batch() {
    assert_proxy_define_batch::<1>();
    assert_proxy_define_batch::<2>();
    assert_proxy_define_batch::<4>();
    assert_proxy_define_batch::<8>();
    assert_proxy_define_batch::<16>();
}

/// Forces the outer parent continuation through an inner getOwnPropertyDescriptor trap.
#[test]
fn proxy_define_nested_target_descriptor_resumes_under_forced_major() {
    let module = nested_proxy_define_module();
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(16 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .unwrap();
    let code = isolate.load_module(&module).unwrap();
    let outer_trap = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(1));
    let inner_trap = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(2));
    let leaf = isolate.create_ordinary_object().unwrap();
    let inner_handler = isolate.create_ordinary_object().unwrap();
    let get_own = isolate
        .intern_intrinsic_name(b"getOwnPropertyDescriptor")
        .unwrap();
    isolate
        .set_own_data_property(inner_handler, get_own, inner_trap)
        .unwrap();
    isolate.fiber.registers = vec![leaf, inner_handler];
    let inner = isolate
        .create_proxy_from_site(&proxy_call_site(&isolate, 2))
        .unwrap();
    let outer_handler = isolate.create_ordinary_object().unwrap();
    let define_property = isolate.intern_intrinsic_name(b"defineProperty").unwrap();
    isolate
        .set_own_data_property(outer_handler, define_property, outer_trap)
        .unwrap();
    isolate.fiber.registers = vec![inner, outer_handler];
    let outer = isolate
        .create_proxy_from_site(&proxy_call_site(&isolate, 2))
        .unwrap();
    let descriptor = isolate.create_ordinary_object().unwrap();
    let key_atom = isolate.intern_intrinsic_name(b"nested").unwrap();
    let key = isolate.atom_string_value(key_atom).unwrap();
    for (name, value) in [
        (b"proxy".as_slice(), outer),
        (b"key".as_slice(), key),
        (b"descriptor".as_slice(), descriptor),
    ] {
        let atom = isolate.intern_intrinsic_name(name).unwrap();
        isolate.realm.set(atom, value).unwrap();
    }
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
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

/// Exercises descriptor parsing, FromPropertyDescriptor, trap lookup, and three arguments under GC.
fn assert_proxy_define_batch<const N: usize>() {
    let module = proxy_define_module();
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(16 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .unwrap();
    let code = isolate.load_module(&module).unwrap();
    let value_getter = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(1));
    let trap_getter = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(2));
    let trap = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(3));
    let target = isolate.create_ordinary_object().unwrap();
    let handler = isolate.create_ordinary_object().unwrap();
    let descriptor = isolate.create_ordinary_object().unwrap();
    let expected_value = isolate.create_ordinary_object().unwrap();
    let define_property = isolate.intern_intrinsic_name(b"defineProperty").unwrap();
    let value = isolate.intern_intrinsic_name(b"value").unwrap();
    isolate
        .define_property(
            handler,
            define_property.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(trap_getter),
                setter: None,
                enumerable: Some(true),
                configurable: Some(true),
            }),
        )
        .unwrap();
    isolate
        .define_property(
            descriptor,
            value.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(value_getter),
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
        (b"descriptor".as_slice(), descriptor),
        (b"expectedValue".as_slice(), expected_value),
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
                fuel: 192,
                quantum: 192,
            },
        )
        .unwrap();
    assert_eq!(
        outcome,
        RunOutcome::Completed(Value::from_immediate(Immediate::True))
    );
}

/// Builds a Reflect.defineProperty call with accessor-backed descriptor and trap lookups.
fn proxy_define_module() -> CompiledModule {
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
    entry.emit(Opcode::Return, &[10], span).unwrap();
    let (entry_bytecode, entry_map, entry_registers) = entry.finish().unwrap();

    let mut value_getter = BytecodeBuilder::default();
    value_getter.emit(Opcode::LoadScope, &[0, 5], span).unwrap();
    value_getter.emit(Opcode::Return, &[0], span).unwrap();
    let (value_getter_bytecode, value_getter_map, value_getter_registers) =
        value_getter.finish().unwrap();

    let mut trap_getter = BytecodeBuilder::default();
    trap_getter.emit(Opcode::LoadScope, &[0, 6], span).unwrap();
    trap_getter.emit(Opcode::Return, &[0], span).unwrap();
    let (trap_getter_bytecode, trap_getter_map, trap_getter_registers) =
        trap_getter.finish().unwrap();

    let mut trap = BytecodeBuilder::default();
    trap.emit(Opcode::LoadThis, &[3], span).unwrap();
    trap.emit(Opcode::LoadScope, &[4, 7], span).unwrap();
    trap.emit(Opcode::StrictEqual, &[5, 0, 4], span).unwrap();
    trap.emit(Opcode::LoadScope, &[6, 3], span).unwrap();
    trap.emit(Opcode::StrictEqual, &[7, 1, 6], span).unwrap();
    trap.emit(Opcode::GetById, &[8, 2, 9], span).unwrap();
    trap.emit(Opcode::LoadScope, &[9, 5], span).unwrap();
    trap.emit(Opcode::StrictEqual, &[10, 8, 9], span).unwrap();
    trap.emit(Opcode::LoadScope, &[11, 8], span).unwrap();
    trap.emit(Opcode::StrictEqual, &[12, 3, 11], span).unwrap();
    trap.emit(Opcode::BitwiseAnd, &[13, 5, 7], span).unwrap();
    trap.emit(Opcode::BitwiseAnd, &[14, 13, 10], span).unwrap();
    trap.emit(Opcode::BitwiseAnd, &[15, 14, 12], span).unwrap();
    trap.emit(Opcode::Return, &[15], span).unwrap();
    let (trap_bytecode, trap_map, trap_registers) = trap.finish().unwrap();

    CompiledModule::new(
        Arc::from("proxy define continuation"),
        Vec::new(),
        vec![
            Arc::from("Reflect"),
            Arc::from("defineProperty"),
            Arc::from("proxy"),
            Arc::from("key"),
            Arc::from("descriptor"),
            Arc::from("expectedValue"),
            Arc::from("trap"),
            Arc::from("expectedTarget"),
            Arc::from("expectedHandler"),
            Arc::from("value"),
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
                value_getter_bytecode,
                value_getter_map,
                value_getter_registers,
                0,
            ),
            proxy_test_template(
                FunctionId::new(2),
                trap_getter_bytecode,
                trap_getter_map,
                trap_getter_registers,
                0,
            ),
            proxy_test_template(
                FunctionId::new(3),
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

/// Builds a define trap whose target descriptor is supplied by a nested Proxy callback.
fn nested_proxy_define_module() -> CompiledModule {
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
    entry.emit(Opcode::Return, &[10], span).unwrap();
    let (entry_bytecode, entry_map, entry_registers) = entry.finish().unwrap();

    let mut outer_trap = BytecodeBuilder::default();
    outer_trap.emit(Opcode::LoadTrue, &[3], span).unwrap();
    outer_trap.emit(Opcode::Return, &[3], span).unwrap();
    let (outer_bytecode, outer_map, outer_registers) = outer_trap.finish().unwrap();

    let mut inner_trap = BytecodeBuilder::default();
    inner_trap.emit(Opcode::LoadUndefined, &[2], span).unwrap();
    inner_trap.emit(Opcode::Return, &[2], span).unwrap();
    let (inner_bytecode, inner_map, inner_registers) = inner_trap.finish().unwrap();

    CompiledModule::new(
        Arc::from("nested proxy define invariant"),
        Vec::new(),
        vec![
            Arc::from("Reflect"),
            Arc::from("defineProperty"),
            Arc::from("proxy"),
            Arc::from("key"),
            Arc::from("descriptor"),
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
                outer_bytecode,
                outer_map,
                outer_registers,
                3,
            ),
            proxy_test_template(
                FunctionId::new(2),
                inner_bytecode,
                inner_map,
                inner_registers,
                2,
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}
