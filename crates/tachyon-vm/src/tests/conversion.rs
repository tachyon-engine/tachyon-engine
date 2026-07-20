use super::{fixtures::test_isolate, *};

#[test]
fn exotic_getter_and_method_resume_for_every_dispatch_batch() {
    assert_exotic_conversion_batch::<1>();
    assert_exotic_conversion_batch::<2>();
    assert_exotic_conversion_batch::<4>();
    assert_exotic_conversion_batch::<8>();
    assert_exotic_conversion_batch::<16>();
}

#[test]
/// The exotic getter call needs one parent entry and its returned callable needs one child root.
fn exotic_conversion_respects_two_entry_completion_limit() {
    for (limit, expected) in [(1, false), (2, true)] {
        let module = exotic_conversion_module();
        let mut isolate = test_isolate();
        install_exotic_getter(&mut isolate, &module, false);
        isolate.stack_limits = StackLimits::new(64, 4_096).with_max_completions(limit);
        let result = isolate.execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 64,
                quantum: 64,
            },
        );
        if expected {
            assert!(
                matches!(result, Ok(RunOutcome::Completed(value)) if value.as_i32() == Some(42))
            );
        } else {
            assert_eq!(
                result,
                Err(ExecutionError::CompletionStackLimit {
                    limit: 1,
                    requested: 2,
                })
            );
            assert_eq!(isolate.fiber.completions.len(), 0);
        }
    }
}

/// Runs an own and inherited symbol getter whose fresh closure consumes the exact default hint.
fn assert_exotic_conversion_batch<const N: usize>() {
    for inherited in [false, true] {
        let module = exotic_conversion_module();
        let mut isolate = test_isolate();
        install_exotic_getter(&mut isolate, &module, inherited);
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
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }
}

/// Publishes an own or inherited `@@toPrimitive` getter and the receiver global used by its method.
fn install_exotic_getter(isolate: &mut Isolate, module: &CompiledModule, inherited: bool) {
    let code = isolate.load_module(module).unwrap();
    let getter = allocate_test_function(isolate, code, FunctionId::new(1));
    let owner = isolate.create_ordinary_object().unwrap();
    let target = if inherited {
        isolate
            .create_ordinary_object_with_prototype(owner)
            .unwrap()
    } else {
        owner
    };
    let target_name = isolate.intern_intrinsic_name(b"target").unwrap();
    isolate.realm.set(target_name, target).unwrap();
    let symbol = isolate
        .realm
        .well_known_symbols
        .to_primitive
        .expect("realm initialization publishes Symbol.toPrimitive");
    let key = isolate.property_key(symbol).unwrap();
    isolate
        .define_property(
            owner,
            key,
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(getter),
                setter: None,
                enumerable: Some(false),
                configurable: Some(true),
            }),
        )
        .unwrap();
}

/// Allocates one bytecode callback directly so the test can control its GC and continuation state.
fn allocate_test_function(isolate: &mut Isolate, code: CodeId, function: FunctionId) -> Value {
    let prototype = isolate.realm.function_prototype.unwrap();
    let roots = &mut VmRoots {
        fiber: &mut isolate.fiber,
        finalization_jobs: &mut isolate.finalization_jobs,
        realm: &mut isolate.realm,
        loaded_code: &mut isolate.loaded_code,
    };
    isolate
        .heap
        .try_allocate_with_gc(
            isolate.types.function,
            0,
            0,
            FunctionObject {
                executable: FunctionExecutable::Bytecode {
                    code,
                    function,
                    environment: None,
                },
                function_prototype: None,
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

/// Builds `40 + target`, a getter returning a fresh closure, and a method validating hint plus this.
fn exotic_conversion_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(4, 0);
    entry.emit(Opcode::LoadImmediate, &[0, 40], span).unwrap();
    entry.emit(Opcode::LoadScope, &[1, 0], span).unwrap();
    entry.emit(Opcode::Add, &[2, 0, 1], span).unwrap();
    entry.emit(Opcode::Return, &[2], span).unwrap();
    let (entry_code, entry_map, entry_registers) = entry.finish().unwrap();

    let mut getter = BytecodeBuilder::with_capacity(9, 0);
    let valid_receiver = getter.new_label().unwrap();
    getter.emit(Opcode::LoadThis, &[0], span).unwrap();
    getter.emit(Opcode::LoadScope, &[1, 0], span).unwrap();
    getter.emit(Opcode::StrictEqual, &[2, 0, 1], span).unwrap();
    getter
        .emit_jump_if_true(RegisterId::new(2), valid_receiver, span)
        .unwrap();
    getter.emit(Opcode::LoadImmediate, &[3, 0], span).unwrap();
    getter.emit(Opcode::Return, &[3], span).unwrap();
    getter.bind_label(valid_receiver).unwrap();
    getter.emit(Opcode::CreateObject, &[3], span).unwrap();
    getter.emit(Opcode::CreateClosure, &[4, 2], span).unwrap();
    getter.emit(Opcode::Return, &[4], span).unwrap();
    let (getter_code, getter_map, getter_registers) = getter.finish().unwrap();

    let mut method = BytecodeBuilder::with_capacity(9, 0);
    method.emit(Opcode::CreateObject, &[1], span).unwrap();
    method.emit(Opcode::LoadConstant, &[2, 0], span).unwrap();
    method.emit(Opcode::StrictEqual, &[3, 0, 2], span).unwrap();
    method.emit(Opcode::LoadThis, &[4], span).unwrap();
    method.emit(Opcode::LoadScope, &[5, 0], span).unwrap();
    method.emit(Opcode::StrictEqual, &[6, 4, 5], span).unwrap();
    method.emit(Opcode::Add, &[7, 3, 6], span).unwrap();
    method.emit(Opcode::Return, &[7], span).unwrap();
    let (method_code, method_map, method_registers) = method.finish().unwrap();

    let template = |id, code, source_map, register_count, argument_count| {
        CompiledFunctionTemplate::new(
            FunctionId::new(id),
            code,
            FunctionMetadata {
                layout: FunctionLayout {
                    register_count,
                    argument_count,
                    ..FunctionLayout::default()
                },
                source_map,
                ..FunctionMetadata::new(
                    if id == 0 {
                        FunctionKind::Script
                    } else {
                        FunctionKind::Ordinary
                    },
                    FunctionLayout::default(),
                )
            },
        )
    };
    CompiledModule::new(
        Arc::from("resumable Symbol.toPrimitive"),
        vec![BytecodeConstant::string_from_utf16(
            "default".encode_utf16().collect(),
        )],
        vec![Arc::from("target")],
        vec![
            template(0, entry_code, entry_map, entry_registers, 0),
            template(1, getter_code, getter_map, getter_registers, 0),
            template(2, method_code, method_map, method_registers, 1),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}
