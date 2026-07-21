use super::{fixtures::test_isolate, *};

mod define;
mod delete;
mod get;
mod get_own;

fn proxy_call_site(isolate: &Isolate, argument_count: u32) -> CallSite {
    CallSite {
        caller_base: 0,
        destination: 0,
        callee: isolate.realm.proxy_constructor.unwrap(),
        argument_base: 0,
        argument_source: None,
        argument_prefix: None,
        argument_prefix_offset: 0,
        argument_prefix_count: 0,
        argument_count,
        this_value: Value::from_immediate(Immediate::Undefined),
        new_target: isolate.realm.proxy_constructor.unwrap(),
        construct_receiver: None,
        call_site: WordOffset::new(0),
    }
}

#[test]
/// Roots both ProxyCreate inputs and preserves their exact identities through a later major GC.
fn proxy_create_payload_survives_forced_major() {
    let mut isolate = test_isolate();
    let target = isolate.create_ordinary_object().unwrap();
    let handler = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers = vec![target, handler];
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let proxy = isolate
        .create_proxy_from_site(&proxy_call_site(&isolate, 2))
        .unwrap();
    isolate.fiber.registers = vec![proxy];
    isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"collect").unwrap())
        .unwrap();
    let raw = proxy.as_heap_ref().unwrap();
    let proxy = isolate
        .heap
        .checked_reference(raw, isolate.types.proxy_object)
        .unwrap();
    let snapshot = isolate.heap.with_running_scope(|scope| {
        let proxy = scope.root(proxy).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow(proxy, isolate.types.proxy_object)
                .copied()
                .unwrap()
        })
    });
    assert_eq!((snapshot.target, snapshot.handler), (target, handler));
    assert!(isolate.is_object_value(Value::from_heap_ref(proxy.raw())));
}

#[test]
fn proxy_constructor_validates_arguments_and_has_no_default_prototype() {
    let mut isolate = test_isolate();
    let target = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers = vec![target, Value::from_immediate(Immediate::Null)];
    assert!(matches!(
        isolate.create_proxy_from_site(&proxy_call_site(&isolate, 2)),
        Err(ExecutionError::NotObject(value))
            if value.as_immediate() == Some(Immediate::Null)
    ));
    let prototype = isolate.prototype_atom().unwrap();
    assert!(
        !isolate
            .is_function_prototype_property(isolate.realm.proxy_constructor.unwrap(), prototype,)
    );
}

#[test]
/// A failed private brand read must not allocate or publish a Proxy sidecar.
fn failed_private_brand_read_keeps_proxy_sidecar_absent() {
    let mut isolate = test_isolate();
    let target = isolate.create_ordinary_object().unwrap();
    let handler = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers = vec![target, handler];
    let proxy = isolate
        .create_proxy_from_site(&proxy_call_site(&isolate, 2))
        .unwrap();
    isolate.fiber.registers = vec![proxy];
    let private_name = isolate.allocate_symbol(None).unwrap();
    isolate.fiber.registers.push(private_name);
    let proxy = isolate.fiber.registers[0];
    assert!(matches!(
        isolate.get_private_field(proxy, private_name),
        Err(ExecutionError::PrivateBrandCheckFailed(value)) if value == proxy
    ));
    assert!(
        isolate
            .proxy_snapshot(proxy)
            .unwrap()
            .private_storage
            .is_none()
    );
}

#[test]
/// Keeps revocable construction rooted, then clears every private edge exactly once.
fn proxy_revoker_survives_forced_major_and_is_idempotent() {
    assert_eq!(core::mem::size_of::<FunctionExecutable>(), 16);
    let mut isolate = test_isolate();
    let target = isolate.create_ordinary_object().unwrap();
    let handler = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers = vec![target, handler];
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let site = proxy_call_site(&isolate, 2);
    let result = isolate.create_revocable_proxy_from_site(&site).unwrap();
    isolate.fiber.registers = vec![result];
    let proxy_atom = isolate.intern_intrinsic_name(b"proxy").unwrap();
    let revoke_atom = isolate.intern_intrinsic_name(b"revoke").unwrap();
    let proxy = isolate
        .get_data_property(result, proxy_atom)
        .unwrap()
        .unwrap();
    let revoker = isolate
        .get_data_property(result, revoke_atom)
        .unwrap()
        .unwrap();
    isolate.fiber.registers = vec![result, proxy, revoker];
    let revoke_site = CallSite {
        caller_base: 0,
        destination: 0,
        callee: revoker,
        argument_base: 0,
        argument_source: None,
        argument_prefix: None,
        argument_prefix_offset: 0,
        argument_prefix_count: 0,
        argument_count: 0,
        this_value: Value::from_immediate(Immediate::Undefined),
        new_target: Value::from_immediate(Immediate::Undefined),
        construct_receiver: None,
        call_site: WordOffset::new(0),
    };
    isolate.call(revoke_site).unwrap();
    assert_eq!(
        isolate.fiber.registers[0].as_immediate(),
        Some(Immediate::Undefined)
    );
    let snapshot = isolate.proxy_snapshot(proxy).unwrap();
    assert_eq!(snapshot.target.as_immediate(), Some(Immediate::Null));
    assert_eq!(snapshot.handler.as_immediate(), Some(Immediate::Null));
    isolate.call(revoke_site).unwrap();
    assert!(matches!(
        isolate.resolve_function_object(revoker).unwrap().executable,
        FunctionExecutable::ProxyRevoker(value)
            if value.as_immediate() == Some(Immediate::Null)
    ));
}

#[test]
fn proxy_trap_getter_and_call_resume_for_every_dispatch_batch() {
    assert_proxy_trap_continuation_batch::<1>();
    assert_proxy_trap_continuation_batch::<2>();
    assert_proxy_trap_continuation_batch::<4>();
    assert_proxy_trap_continuation_batch::<8>();
    assert_proxy_trap_continuation_batch::<16>();
}

#[test]
fn proxy_set_prototype_arguments_resume_for_every_dispatch_batch() {
    assert_proxy_set_prototype_batch::<1>();
    assert_proxy_set_prototype_batch::<2>();
    assert_proxy_set_prototype_batch::<4>();
    assert_proxy_set_prototype_batch::<8>();
    assert_proxy_set_prototype_batch::<16>();
}

#[test]
fn proxy_has_arguments_resume_for_every_dispatch_batch() {
    assert_proxy_has_batch::<1>();
    assert_proxy_has_batch::<2>();
    assert_proxy_has_batch::<4>();
    assert_proxy_has_batch::<8>();
    assert_proxy_has_batch::<16>();
}

#[test]
fn proxy_has_missing_and_nullish_traps_forward_while_noncallable_rejects() {
    let mut isolate = test_isolate();
    let key_atom = isolate.intern_intrinsic_name(b"visible").unwrap();
    let key = isolate.atom_string_value(key_atom).unwrap();
    let target = isolate.create_ordinary_object().unwrap();
    isolate
        .set_own_data_property(target, key_atom, Value::from_i32(1))
        .unwrap();
    let inner_handler = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers = vec![target, inner_handler];
    let inner = isolate
        .create_proxy_from_site(&proxy_call_site(&isolate, 2))
        .unwrap();
    let outer_handler = isolate.create_ordinary_object().unwrap();
    let has_atom = isolate.intern_intrinsic_name(b"has").unwrap();
    isolate
        .set_own_data_property(
            outer_handler,
            has_atom,
            Value::from_immediate(Immediate::Null),
        )
        .unwrap();
    isolate.fiber.registers = vec![inner, outer_handler];
    let outer = isolate
        .create_proxy_from_site(&proxy_call_site(&isolate, 2))
        .unwrap();
    isolate.fiber.registers = vec![Value::from_immediate(Immediate::Undefined)];
    isolate
        .dispatch_proxy_has(
            NativeContinuationSite {
                caller_base: 0,
                destination: 0,
                call_site: WordOffset::new(0),
            },
            outer,
            key,
        )
        .unwrap();
    assert_eq!(
        isolate.fiber.registers[0].as_immediate(),
        Some(Immediate::True)
    );
    let child = isolate
        .create_ordinary_object_with_prototype(outer)
        .unwrap();
    isolate
        .dispatch_has_property(
            NativeContinuationSite {
                caller_base: 0,
                destination: 0,
                call_site: WordOffset::new(0),
            },
            child,
            key,
        )
        .unwrap();
    assert_eq!(
        isolate.fiber.registers[0].as_immediate(),
        Some(Immediate::True)
    );

    isolate
        .set_own_data_property(
            outer_handler,
            has_atom,
            Value::from_immediate(Immediate::True),
        )
        .unwrap();
    assert!(matches!(
        isolate.dispatch_proxy_has(
            NativeContinuationSite {
                caller_base: 0,
                destination: 0,
                call_site: WordOffset::new(0),
            },
            outer,
            key,
        ),
        Err(ExecutionError::NonCallable(value))
            if value.as_immediate() == Some(Immediate::True)
    ));
}

#[test]
fn proxy_has_false_rejects_nonconfigurable_and_nonextensible_target_properties() {
    for nonconfigurable in [true, false] {
        let module = proxy_has_false_module();
        let mut isolate = test_isolate();
        let code = isolate.load_module(&module).unwrap();
        let trap = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(1));
        let target = isolate.create_ordinary_object().unwrap();
        let key_atom = isolate.intern_intrinsic_name(b"visible").unwrap();
        isolate
            .define_property(
                target,
                key_atom.into(),
                PropertyDescriptor::Data(DataPropertyDescriptor {
                    value: Some(Value::from_i32(1)),
                    writable: Some(true),
                    enumerable: Some(true),
                    configurable: Some(!nonconfigurable),
                }),
            )
            .unwrap();
        if !nonconfigurable {
            let receiver = isolate.object_snapshot(target).unwrap().0;
            isolate.set_object_extensible(receiver, false).unwrap();
        }
        let handler = isolate.create_ordinary_object().unwrap();
        let has_atom = isolate.intern_intrinsic_name(b"has").unwrap();
        isolate
            .set_own_data_property(handler, has_atom, trap)
            .unwrap();
        isolate.fiber.registers = vec![target, handler];
        let proxy = isolate
            .create_proxy_from_site(&proxy_call_site(&isolate, 2))
            .unwrap();
        let key = isolate.atom_string_value(key_atom).unwrap();
        for (name, value) in [(b"proxy".as_slice(), proxy), (b"key".as_slice(), key)] {
            let atom = isolate.intern_intrinsic_name(name).unwrap();
            isolate.realm.set(atom, value).unwrap();
        }
        let outcome = isolate
            .execute_with_batch::<8>(
                &module,
                ExecutionBudget {
                    fuel: 64,
                    quantum: 64,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Thrown(value)
                if isolate.native_error_kind(value).unwrap() == Some(NativeErrorKind::Type)
        ));
    }
}

/// Verifies accessor lookup, handler `this`, `(target, key)` arguments, and forced-major rooting.
fn assert_proxy_has_batch<const N: usize>() {
    let module = proxy_has_module();
    let mut isolate = test_isolate();
    let code = isolate.load_module(&module).unwrap();
    let getter = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(1));
    let trap = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(2));
    let target = isolate.create_ordinary_object().unwrap();
    let handler = isolate.create_ordinary_object().unwrap();
    let has = isolate.intern_intrinsic_name(b"has").unwrap();
    isolate
        .define_property(
            handler,
            has.into(),
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
    let expected_key = isolate.atom_string_value(has).unwrap();
    for (name, value) in [
        (b"proxy".as_slice(), proxy),
        (b"trap".as_slice(), trap),
        (b"expectedTarget".as_slice(), target),
        (b"expectedKey".as_slice(), expected_key),
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
                fuel: 96,
                quantum: 96,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

/// Verifies the traced native argument source, handler receiver, and invariant after forced major GC.
fn assert_proxy_set_prototype_batch<const N: usize>() {
    let module = proxy_set_prototype_module();
    let mut isolate = test_isolate();
    let code = isolate.load_module(&module).unwrap();
    let getter = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(1));
    let trap = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(2));
    let target = isolate.create_ordinary_object().unwrap();
    let prototype = isolate.create_ordinary_object().unwrap();
    let handler = isolate.create_ordinary_object().unwrap();
    assert!(
        isolate
            .ordinary_set_prototype_of(target, prototype)
            .unwrap()
    );
    let receiver = isolate.object_snapshot(target).unwrap().0;
    isolate.set_object_extensible(receiver, false).unwrap();
    let key = isolate.intern_intrinsic_name(b"setPrototypeOf").unwrap();
    isolate
        .define_property(
            handler,
            key.into(),
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
        (b"trap".as_slice(), trap),
        (b"expectedTarget".as_slice(), target),
        (b"expectedPrototype".as_slice(), prototype),
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
                fuel: 96,
                quantum: 96,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::True)
    ));
}

/// Runs an accessor-backed isExtensible trap through two bytecode callbacks and forced major GC.
fn assert_proxy_trap_continuation_batch<const N: usize>() {
    let module = proxy_is_extensible_module();
    let mut isolate = test_isolate();
    let code = isolate.load_module(&module).unwrap();
    let getter = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(1));
    let trap = allocate_proxy_test_function(&mut isolate, code, FunctionId::new(2));
    let trap_atom = isolate.intern_intrinsic_name(b"trap").unwrap();
    isolate.realm.set(trap_atom, trap).unwrap();
    let target = isolate.create_ordinary_object().unwrap();
    let handler = isolate.create_ordinary_object().unwrap();
    let key = isolate.intern_intrinsic_name(b"isExtensible").unwrap();
    isolate
        .define_property(
            handler,
            key.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(getter),
                setter: None,
                enumerable: Some(true),
                configurable: Some(true),
            }),
        )
        .unwrap();
    isolate.fiber.registers = vec![target, handler];
    let site = proxy_call_site(&isolate, 2);
    let proxy = isolate.create_proxy_from_site(&site).unwrap();
    let proxy_atom = isolate.intern_intrinsic_name(b"proxy").unwrap();
    isolate.realm.set(proxy_atom, proxy).unwrap();
    let trap_value = isolate
        .realm
        .resolve(trap_atom)
        .and_then(|slot| isolate.realm.get_slot(slot))
        .or_else(|| {
            isolate
                .realm
                .resolve_intrinsic(trap_atom)
                .map(|slot| isolate.realm.intrinsic_value(slot))
        });
    assert_eq!(trap_value, Some(trap));
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 64,
                quantum: 64,
            },
        )
        .unwrap();
    let thrown_kind = match outcome {
        RunOutcome::Thrown(value) => isolate.native_error_kind(value).unwrap(),
        RunOutcome::Completed(_) | RunOutcome::BudgetExhausted => None,
    };
    assert!(
        matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ),
        "unexpected proxy continuation outcome: {outcome:?}, error kind: {thrown_kind:?}"
    );
}

/// Builds `Reflect.isExtensible(proxy)` plus bytecode trap-getter and trap functions.
fn proxy_is_extensible_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(5, 0);
    entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    entry.emit(Opcode::GetById, &[1, 0, 1], span).unwrap();
    entry.emit(Opcode::LoadScope, &[2, 2], span).unwrap();
    entry.emit(Opcode::Call, &[3, 1, 1], span).unwrap();
    entry.emit(Opcode::Return, &[3], span).unwrap();
    let (entry_bytecode, entry_map, entry_registers) = entry.finish().unwrap();
    let mut getter = BytecodeBuilder::with_capacity(2, 0);
    getter.emit(Opcode::LoadScope, &[0, 3], span).unwrap();
    getter.emit(Opcode::Return, &[0], span).unwrap();
    let (getter_bytecode, getter_map, getter_registers) = getter.finish().unwrap();
    let mut trap = BytecodeBuilder::with_capacity(2, 0);
    trap.emit(Opcode::LoadTrue, &[0], span).unwrap();
    trap.emit(Opcode::Return, &[0], span).unwrap();
    let (trap_bytecode, trap_map, trap_registers) = trap.finish().unwrap();
    let templates = vec![
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
            1,
        ),
    ];
    CompiledModule::new(
        Arc::from("proxy isExtensible continuation"),
        Vec::new(),
        vec![
            Arc::from("Reflect"),
            Arc::from("isExtensible"),
            Arc::from("proxy"),
            Arc::from("trap"),
        ],
        templates,
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds an accessor-backed `Reflect.setPrototypeOf(proxy, prototype)` argument identity check.
fn proxy_set_prototype_module() -> CompiledModule {
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
    trap.emit(Opcode::LoadScope, &[5, 6], span).unwrap();
    trap.emit(Opcode::StrictEqual, &[6, 1, 5], span).unwrap();
    trap.emit(Opcode::LoadScope, &[7, 7], span).unwrap();
    trap.emit(Opcode::StrictEqual, &[8, 2, 7], span).unwrap();
    trap.emit(Opcode::BitwiseAnd, &[9, 4, 6], span).unwrap();
    trap.emit(Opcode::BitwiseAnd, &[10, 9, 8], span).unwrap();
    trap.emit(Opcode::CreateObject, &[11], span).unwrap();
    trap.emit(Opcode::Return, &[10], span).unwrap();
    let (trap_bytecode, trap_map, trap_registers) = trap.finish().unwrap();
    let templates = vec![
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
    ];
    CompiledModule::new(
        Arc::from("proxy setPrototypeOf continuation"),
        Vec::new(),
        vec![
            Arc::from("Reflect"),
            Arc::from("setPrototypeOf"),
            Arc::from("proxy"),
            Arc::from("expectedPrototype"),
            Arc::from("trap"),
            Arc::from("expectedTarget"),
            Arc::from("expectedPrototype"),
            Arc::from("expectedHandler"),
        ],
        templates,
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds an accessor-backed `Reflect.has(proxy, key)` argument and receiver identity check.
fn proxy_has_module() -> CompiledModule {
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
    trap.emit(Opcode::LoadScope, &[5, 6], span).unwrap();
    trap.emit(Opcode::StrictEqual, &[6, 1, 5], span).unwrap();
    trap.emit(Opcode::LoadScope, &[7, 7], span).unwrap();
    trap.emit(Opcode::StrictEqual, &[8, 2, 7], span).unwrap();
    trap.emit(Opcode::BitwiseAnd, &[9, 4, 6], span).unwrap();
    trap.emit(Opcode::BitwiseAnd, &[10, 9, 8], span).unwrap();
    trap.emit(Opcode::Return, &[10], span).unwrap();
    let (trap_bytecode, trap_map, trap_registers) = trap.finish().unwrap();
    let templates = vec![
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
    ];
    CompiledModule::new(
        Arc::from("proxy has continuation"),
        Vec::new(),
        vec![
            Arc::from("Reflect"),
            Arc::from("has"),
            Arc::from("proxy"),
            Arc::from("expectedKey"),
            Arc::from("trap"),
            Arc::from("expectedTarget"),
            Arc::from("expectedKey"),
            Arc::from("expectedHandler"),
        ],
        templates,
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds `key in proxy` with a false-returning trap for exact invariant checks.
fn proxy_has_false_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::default();
    entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    entry.emit(Opcode::LoadScope, &[1, 1], span).unwrap();
    entry.emit(Opcode::HasProperty, &[2, 1, 0], span).unwrap();
    entry.emit(Opcode::Return, &[2], span).unwrap();
    let (entry_bytecode, entry_map, entry_registers) = entry.finish().unwrap();
    let mut trap = BytecodeBuilder::default();
    trap.emit(Opcode::LoadFalse, &[2], span).unwrap();
    trap.emit(Opcode::Return, &[2], span).unwrap();
    let (trap_bytecode, trap_map, trap_registers) = trap.finish().unwrap();
    CompiledModule::new(
        Arc::from("proxy has false invariant"),
        Vec::new(),
        vec![Arc::from("proxy"), Arc::from("key")],
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

fn proxy_test_template(
    id: FunctionId,
    bytecode: Bytecode,
    source_map: Arc<[SourceMapEntry]>,
    register_count: u32,
    argument_count: u32,
) -> CompiledFunctionTemplate {
    let kind = if id == FunctionId::new(0) {
        FunctionKind::Script
    } else {
        FunctionKind::Ordinary
    };
    CompiledFunctionTemplate::new(
        id,
        bytecode,
        FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                argument_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(kind, FunctionLayout::default())
        },
    )
}

/// Allocates one bytecode function whose immutable code owner is already isolate-rooted.
fn allocate_proxy_test_function(
    isolate: &mut Isolate,
    code: CodeId,
    function: FunctionId,
) -> Value {
    let function_type = isolate.types.function;
    let prototype = isolate.realm.function_prototype.unwrap();
    let roots = &mut VmRoots {
        fiber: &mut isolate.fiber,
        finalization_jobs: &mut isolate.finalization_jobs,
        promise_jobs: &mut isolate.promise_jobs,
        realm: &mut isolate.realm,
        loaded_code: &mut isolate.loaded_code,
    };
    isolate
        .heap
        .try_allocate_with_gc(
            function_type,
            0,
            0,
            FunctionObject {
                executable: FunctionExecutable::Bytecode {
                    code,
                    function,
                    environment: None,
                },
                prototype_or_home_object: None,
                ordinary: OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype,
                },
            },
            AllocationSpace::Young,
            roots,
        )
        .map(|function| Value::from_heap_ref(function.raw()))
        .unwrap()
}
