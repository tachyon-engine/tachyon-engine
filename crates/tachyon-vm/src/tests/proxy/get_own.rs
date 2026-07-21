//! Proxy `[[GetOwnProperty]]` continuation and forced-GC tests.

use super::*;

#[test]
fn proxy_get_own_descriptor_resumes_for_every_dispatch_batch() {
    assert_proxy_get_own_batch::<1>();
    assert_proxy_get_own_batch::<2>();
    assert_proxy_get_own_batch::<4>();
    assert_proxy_get_own_batch::<8>();
    assert_proxy_get_own_batch::<16>();
}

#[test]
/// Forces both nested target internal methods to suspend below the outer descriptor continuation.
fn proxy_get_own_nested_target_methods_resume_under_forced_major() {
    let module = proxy_nested_get_own_module();
    let mut isolate = test_isolate();
    let code = isolate.load_module(&module).unwrap();
    let outer_trap = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(1));
    let inner_get_own = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(2));
    let inner_is_extensible = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(3));
    let target = isolate.create_ordinary_object().unwrap();
    let inner_handler = isolate.create_ordinary_object().unwrap();
    let get_own = isolate
        .intern_intrinsic_name(b"getOwnPropertyDescriptor")
        .unwrap();
    let is_extensible = isolate.intern_intrinsic_name(b"isExtensible").unwrap();
    isolate
        .set_own_data_property(inner_handler, get_own, inner_get_own)
        .unwrap();
    isolate
        .set_own_data_property(inner_handler, is_extensible, inner_is_extensible)
        .unwrap();
    isolate.fiber.registers = vec![target, inner_handler];
    let inner = isolate
        .create_proxy_from_site(&proxy_call_site(&isolate, 2))
        .unwrap();
    let outer_handler = isolate.create_ordinary_object().unwrap();
    isolate
        .set_own_data_property(outer_handler, get_own, outer_trap)
        .unwrap();
    isolate.fiber.registers = vec![inner, outer_handler];
    let outer = isolate
        .create_proxy_from_site(&proxy_call_site(&isolate, 2))
        .unwrap();
    let descriptor = isolate.create_ordinary_object().unwrap();
    let configurable = isolate.intern_intrinsic_name(b"configurable").unwrap();
    let value = isolate.intern_intrinsic_name(b"value").unwrap();
    isolate
        .set_own_data_property(descriptor, configurable, boolean_value(true))
        .unwrap();
    isolate
        .set_own_data_property(descriptor, value, Value::from_i32(7))
        .unwrap();
    let key_atom = isolate.intern_intrinsic_name(b"nested").unwrap();
    let key = isolate.atom_string_value(key_atom).unwrap();
    for (name, value) in [
        (b"outerProxy".as_slice(), outer),
        (b"key".as_slice(), key),
        (b"outerDescriptor".as_slice(), descriptor),
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
                fuel: 160,
                quantum: 160,
            },
        )
        .unwrap();
    let RunOutcome::Completed(result) = outcome else {
        panic!("nested Proxy descriptor query must complete: {outcome:?}");
    };
    assert_eq!(
        isolate.get_data_property(result, value).unwrap(),
        Some(Value::from_i32(7))
    );
}

/// Runs accessor-backed trap lookup, trap call, and six-field descriptor parsing under major GC.
fn assert_proxy_get_own_batch<const N: usize>() {
    let module = proxy_get_own_module();
    let mut isolate = test_isolate();
    let code = isolate.load_module(&module).unwrap();
    let getter = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(1));
    let trap = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(2));
    let descriptor_getter = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(3));
    let target = isolate.create_ordinary_object().unwrap();
    let handler = isolate.create_ordinary_object().unwrap();
    let descriptor = isolate.create_ordinary_object().unwrap();
    let key_atom = isolate.intern_intrinsic_name(b"visible").unwrap();
    let key = isolate.atom_string_value(key_atom).unwrap();
    for name in [b"configurable".as_slice(), b"writable"] {
        let atom = isolate.intern_intrinsic_name(name).unwrap();
        isolate
            .set_own_data_property(descriptor, atom, boolean_value(true))
            .unwrap();
    }
    let enumerable = isolate.intern_intrinsic_name(b"enumerable").unwrap();
    isolate
        .define_property(
            descriptor,
            enumerable.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(descriptor_getter),
                setter: None,
                enumerable: Some(true),
                configurable: Some(true),
            }),
        )
        .unwrap();
    let value_atom = isolate.intern_intrinsic_name(b"value").unwrap();
    isolate
        .set_own_data_property(descriptor, value_atom, Value::from_i32(7))
        .unwrap();
    let trap_atom = isolate
        .intern_intrinsic_name(b"getOwnPropertyDescriptor")
        .unwrap();
    isolate
        .define_property(
            handler,
            trap_atom.into(),
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
    for (name, value) in [
        (b"proxy".as_slice(), proxy),
        (b"key".as_slice(), key),
        (b"trap".as_slice(), trap),
        (b"descriptor".as_slice(), descriptor),
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
    assert!(matches!(outcome, RunOutcome::Completed(value) if isolate.is_object_value(value)));
}

fn proxy_get_own_module() -> CompiledModule {
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
    trap.emit(Opcode::LoadScope, &[2, 5], span).unwrap();
    trap.emit(Opcode::Return, &[2], span).unwrap();
    let (trap_bytecode, trap_map, trap_registers) = trap.finish().unwrap();
    let mut descriptor_getter = BytecodeBuilder::default();
    descriptor_getter
        .emit(Opcode::CreateObject, &[0], span)
        .unwrap();
    descriptor_getter
        .emit(Opcode::LoadTrue, &[1], span)
        .unwrap();
    descriptor_getter.emit(Opcode::Return, &[1], span).unwrap();
    let (descriptor_getter_bytecode, descriptor_getter_map, descriptor_getter_registers) =
        descriptor_getter.finish().unwrap();
    CompiledModule::new(
        Arc::from("proxy getOwnPropertyDescriptor continuation"),
        Vec::new(),
        vec![
            Arc::from("Reflect"),
            Arc::from("getOwnPropertyDescriptor"),
            Arc::from("proxy"),
            Arc::from("key"),
            Arc::from("trap"),
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
            proxy_test_template(
                FunctionId::new(3),
                descriptor_getter_bytecode,
                descriptor_getter_map,
                descriptor_getter_registers,
                0,
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds an outer descriptor trap whose Proxy target suspends in get-own and extensibility traps.
fn proxy_nested_get_own_module() -> CompiledModule {
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
    let mut outer_trap = BytecodeBuilder::default();
    outer_trap.emit(Opcode::LoadScope, &[2, 4], span).unwrap();
    outer_trap.emit(Opcode::Return, &[2], span).unwrap();
    let (outer_bytecode, outer_map, outer_registers) = outer_trap.finish().unwrap();
    let mut inner_get_own = BytecodeBuilder::default();
    inner_get_own
        .emit(Opcode::LoadUndefined, &[1], span)
        .unwrap();
    inner_get_own
        .emit(Opcode::ReturnUndefined, &[], span)
        .unwrap();
    let (inner_get_bytecode, inner_get_map, inner_get_registers) = inner_get_own.finish().unwrap();
    let mut inner_is_extensible = BytecodeBuilder::default();
    inner_is_extensible
        .emit(Opcode::LoadTrue, &[1], span)
        .unwrap();
    inner_is_extensible
        .emit(Opcode::Return, &[1], span)
        .unwrap();
    let (inner_extensible_bytecode, inner_extensible_map, inner_extensible_registers) =
        inner_is_extensible.finish().unwrap();
    CompiledModule::new(
        Arc::from("nested proxy getOwnPropertyDescriptor continuation"),
        Vec::new(),
        vec![
            Arc::from("Reflect"),
            Arc::from("getOwnPropertyDescriptor"),
            Arc::from("outerProxy"),
            Arc::from("key"),
            Arc::from("outerDescriptor"),
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
                2,
            ),
            proxy_test_template(
                FunctionId::new(2),
                inner_get_bytecode,
                inner_get_map,
                inner_get_registers,
                2,
            ),
            proxy_test_template(
                FunctionId::new(3),
                inner_extensible_bytecode,
                inner_extensible_map,
                inner_extensible_registers,
                1,
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}
