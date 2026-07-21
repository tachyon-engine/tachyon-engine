use super::{fixtures::test_isolate, *};

#[derive(Clone, Copy)]
enum ExoticOperation {
    Add,
    LooseEqual,
    LooseNotEqual,
    PropertyKey,
    PropertyKeyForIn,
    BuiltinHasOwn,
    BuiltinDefineProperty,
}

#[test]
fn exotic_getter_and_method_resume_for_every_dispatch_batch() {
    assert_exotic_conversion_batch::<1>();
    assert_exotic_conversion_batch::<2>();
    assert_exotic_conversion_batch::<4>();
    assert_exotic_conversion_batch::<8>();
    assert_exotic_conversion_batch::<16>();
}

#[test]
fn exotic_equality_resumes_for_every_dispatch_batch() {
    assert_exotic_equality_batch::<1>();
    assert_exotic_equality_batch::<2>();
    assert_exotic_equality_batch::<4>();
    assert_exotic_equality_batch::<8>();
    assert_exotic_equality_batch::<16>();
}

#[test]
fn exotic_property_key_resumes_for_every_dispatch_batch() {
    assert_exotic_property_key_batch::<1>();
    assert_exotic_property_key_batch::<2>();
    assert_exotic_property_key_batch::<4>();
    assert_exotic_property_key_batch::<8>();
    assert_exotic_property_key_batch::<16>();
}

#[test]
fn builtin_property_key_resumes_for_every_dispatch_batch() {
    assert_builtin_property_key_batch::<1>();
    assert_builtin_property_key_batch::<2>();
    assert_builtin_property_key_batch::<4>();
    assert_builtin_property_key_batch::<8>();
    assert_builtin_property_key_batch::<16>();
}

#[test]
/// A callback-returned Symbol must be rooted before defineProperty allocates its storage.
fn builtin_property_key_roots_fresh_symbol_during_forced_major() {
    let module = exotic_conversion_module(ExoticOperation::BuiltinDefineProperty);
    let mut isolate = test_isolate();
    let code = isolate.load_module(&module).unwrap();
    isolate.enter(code, module.entry_function()).unwrap();
    let target = isolate.create_ordinary_object().unwrap();
    let descriptor = isolate.create_ordinary_object().unwrap();
    let value = isolate.intern_intrinsic_name(b"value").unwrap();
    isolate
        .set_own_data_property(descriptor, value, Value::from_i32(42))
        .unwrap();
    let symbol = isolate.allocate_symbol(None).unwrap();
    isolate.write(0, 1, target).unwrap();
    isolate.write(0, 2, descriptor).unwrap();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    isolate
        .finish_builtin_property_key(
            NativeContinuationSite {
                caller_base: 0,
                destination: 0,
                call_site: WordOffset::new(0),
            },
            BuiltinPropertyKeyConsumer::DefineProperty,
            PendingNativePropertyKey::new(
                target,
                descriptor,
                Value::from_immediate(Immediate::Undefined),
            ),
            symbol,
        )
        .unwrap();
    isolate.create_ordinary_object().unwrap();
    let key = isolate.property_key(symbol).unwrap();
    assert!(isolate.has_own_property(target, key).unwrap());
}

#[test]
/// The exotic getter call needs one parent entry and its returned callable needs one child root.
fn exotic_conversion_respects_two_entry_completion_limit() {
    for operation in [
        ExoticOperation::Add,
        ExoticOperation::LooseEqual,
        ExoticOperation::PropertyKey,
        ExoticOperation::PropertyKeyForIn,
        ExoticOperation::BuiltinHasOwn,
        ExoticOperation::BuiltinDefineProperty,
    ] {
        for limit in [1, 2] {
            let module = exotic_conversion_module(operation);
            let mut isolate = test_isolate();
            let code = install_exotic_getter(&mut isolate, &module, false);
            if matches!(operation, ExoticOperation::BuiltinDefineProperty) {
                install_descriptor_getter(&mut isolate, code);
            }
            isolate.stack_limits = StackLimits::new(64, 4_096).with_max_completions(limit);
            let result = isolate.execute_with_batch::<8>(
                &module,
                ExecutionBudget {
                    fuel: 64,
                    quantum: 64,
                },
            );
            if limit == 2 {
                match operation {
                    ExoticOperation::Add
                    | ExoticOperation::PropertyKey
                    | ExoticOperation::PropertyKeyForIn
                    | ExoticOperation::BuiltinDefineProperty => assert!(
                        matches!(result, Ok(RunOutcome::Completed(value)) if value.as_i32() == Some(42))
                    ),
                    ExoticOperation::BuiltinHasOwn => assert!(matches!(
                        result,
                        Ok(RunOutcome::Completed(value))
                            if value.as_immediate() == Some(Immediate::True)
                    )),
                    ExoticOperation::LooseEqual => assert!(matches!(
                        result,
                        Ok(RunOutcome::Completed(value))
                            if value.as_immediate() == Some(Immediate::True)
                    )),
                    ExoticOperation::LooseNotEqual => {
                        unreachable!("limit fixture omits inequality")
                    }
                }
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
}

/// Exercises one-value and two-value builtin pending payloads under every dispatch batch.
fn assert_builtin_property_key_batch<const N: usize>() {
    for operation in [
        ExoticOperation::BuiltinHasOwn,
        ExoticOperation::BuiltinDefineProperty,
    ] {
        for inherited in [false, true] {
            let module = exotic_conversion_module(operation);
            let mut isolate = test_isolate();
            let code = install_exotic_getter(&mut isolate, &module, inherited);
            if matches!(operation, ExoticOperation::BuiltinDefineProperty) {
                install_descriptor_getter(&mut isolate, code);
            }
            isolate
                .heap
                .set_forced_collection_mode(ForcedCollectionMode::Major);
            let outcome = isolate
                .execute_with_batch::<N>(
                    &module,
                    ExecutionBudget {
                        fuel: 160,
                        quantum: 160,
                    },
                )
                .unwrap();
            match operation {
                ExoticOperation::BuiltinHasOwn => assert!(matches!(
                    outcome,
                    RunOutcome::Completed(value)
                        if value.as_immediate() == Some(Immediate::True)
                )),
                ExoticOperation::BuiltinDefineProperty => assert!(matches!(
                    outcome,
                    RunOutcome::Completed(value) if value.as_i32() == Some(42)
                )),
                _ => unreachable!("builtin fixture loop is exhaustive"),
            }
        }
    }
}

/// Exercises string-hint key conversion after a fresh callable and two forced collections.
fn assert_exotic_property_key_batch<const N: usize>() {
    for operation in [
        ExoticOperation::PropertyKey,
        ExoticOperation::PropertyKeyForIn,
    ] {
        for inherited in [false, true] {
            let module = exotic_conversion_module(operation);
            let mut isolate = test_isolate();
            install_exotic_getter(&mut isolate, &module, inherited);
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
            assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
        }
    }
}

/// Runs an own and inherited symbol getter whose fresh closure consumes the exact default hint.
fn assert_exotic_conversion_batch<const N: usize>() {
    for inherited in [false, true] {
        let module = exotic_conversion_module(ExoticOperation::Add);
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

/// Exercises both equality opcodes after a fresh exotic callable and two forced collections.
fn assert_exotic_equality_batch<const N: usize>() {
    for operation in [ExoticOperation::LooseEqual, ExoticOperation::LooseNotEqual] {
        let module = exotic_conversion_module(operation);
        let mut isolate = test_isolate();
        install_exotic_getter(&mut isolate, &module, true);
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
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ));
    }
}

/// Publishes an own or inherited `@@toPrimitive` getter and the receiver global used by its method.
fn install_exotic_getter(
    isolate: &mut Isolate,
    module: &CompiledModule,
    inherited: bool,
) -> CodeId {
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
    code
}

/// Publishes a descriptor whose value getter forces another continuation after key conversion.
fn install_descriptor_getter(isolate: &mut Isolate, code: CodeId) {
    let getter = allocate_test_function(isolate, code, FunctionId::new(3));
    let descriptor = isolate.create_ordinary_object().unwrap();
    let descriptor_name = isolate.intern_intrinsic_name(b"descriptor").unwrap();
    isolate.realm.set(descriptor_name, descriptor).unwrap();
    let value = isolate.intern_intrinsic_name(b"value").unwrap();
    isolate
        .define_property(
            descriptor,
            value.into(),
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
        promise_jobs: &mut isolate.promise_jobs,
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

/// Builds one exotic operation, a fresh getter closure, and a method validating hint plus this.
fn exotic_conversion_module(operation: ExoticOperation) -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(11, 0);
    if matches!(operation, ExoticOperation::BuiltinHasOwn) {
        entry.emit(Opcode::LoadScope, &[0, 1], span).unwrap();
        entry.emit(Opcode::GetById, &[0, 0, 2], span).unwrap();
        entry.emit(Opcode::CreateObject, &[1], span).unwrap();
        entry.emit(Opcode::LoadImmediate, &[4, 2], span).unwrap();
        entry.emit(Opcode::LoadImmediate, &[5, 42], span).unwrap();
        entry.emit(Opcode::SetByValue, &[1, 5, 4], span).unwrap();
        entry.emit(Opcode::LoadScope, &[2, 0], span).unwrap();
        entry.emit(Opcode::Call, &[3, 0, 2], span).unwrap();
        entry.emit(Opcode::Return, &[3], span).unwrap();
    } else if matches!(operation, ExoticOperation::BuiltinDefineProperty) {
        entry.emit(Opcode::LoadScope, &[0, 1], span).unwrap();
        entry.emit(Opcode::GetById, &[0, 0, 2], span).unwrap();
        entry.emit(Opcode::CreateObject, &[1], span).unwrap();
        entry.emit(Opcode::LoadScope, &[2, 0], span).unwrap();
        entry.emit(Opcode::LoadScope, &[3, 3], span).unwrap();
        entry.emit(Opcode::Call, &[4, 0, 3], span).unwrap();
        entry.emit(Opcode::LoadImmediate, &[5, 2], span).unwrap();
        entry.emit(Opcode::GetByValue, &[6, 1, 5], span).unwrap();
        entry.emit(Opcode::Return, &[6], span).unwrap();
    } else if matches!(
        operation,
        ExoticOperation::PropertyKey | ExoticOperation::PropertyKeyForIn
    ) {
        entry.emit(Opcode::CreateObject, &[0], span).unwrap();
        entry.emit(Opcode::LoadScope, &[1, 0], span).unwrap();
        let opcode = if matches!(operation, ExoticOperation::PropertyKeyForIn) {
            Opcode::ToPropertyKeyForIn
        } else {
            Opcode::ToPropertyKey
        };
        entry.emit(opcode, &[1, 1, 0], span).unwrap();
        entry.emit(Opcode::LoadImmediate, &[2, 42], span).unwrap();
        entry.emit(Opcode::SetByValue, &[0, 2, 1], span).unwrap();
        entry.emit(Opcode::GetByValue, &[3, 0, 1], span).unwrap();
        entry.emit(Opcode::Return, &[3], span).unwrap();
    } else {
        let (opcode, primitive) = match operation {
            ExoticOperation::Add => (Opcode::Add, 40),
            ExoticOperation::LooseEqual => (Opcode::LooseEqual, 2),
            ExoticOperation::LooseNotEqual => (Opcode::LooseNotEqual, 3),
            ExoticOperation::PropertyKey | ExoticOperation::PropertyKeyForIn => {
                unreachable!("property key uses its own fixture")
            }
            ExoticOperation::BuiltinHasOwn | ExoticOperation::BuiltinDefineProperty => {
                unreachable!("builtin property key uses its own fixture")
            }
        };
        entry
            .emit(Opcode::LoadImmediate, &[0, primitive], span)
            .unwrap();
        entry.emit(Opcode::LoadScope, &[1, 0], span).unwrap();
        entry.emit(opcode, &[2, 0, 1], span).unwrap();
        entry.emit(Opcode::Return, &[2], span).unwrap();
    }
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

    let mut descriptor_getter = BytecodeBuilder::with_capacity(2, 0);
    descriptor_getter
        .emit(Opcode::LoadImmediate, &[0, 42], span)
        .unwrap();
    descriptor_getter.emit(Opcode::Return, &[0], span).unwrap();
    let (descriptor_getter_code, descriptor_getter_map, descriptor_getter_registers) =
        descriptor_getter.finish().unwrap();

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
    let expected_hint = if matches!(
        operation,
        ExoticOperation::PropertyKey
            | ExoticOperation::PropertyKeyForIn
            | ExoticOperation::BuiltinHasOwn
            | ExoticOperation::BuiltinDefineProperty
    ) {
        "string"
    } else {
        "default"
    };
    CompiledModule::new(
        Arc::from("resumable Symbol.toPrimitive"),
        vec![BytecodeConstant::string_from_utf16(
            expected_hint.encode_utf16().collect(),
        )],
        if matches!(operation, ExoticOperation::BuiltinHasOwn) {
            vec![
                Arc::from("target"),
                Arc::from("Object"),
                Arc::from("hasOwn"),
            ]
        } else if matches!(operation, ExoticOperation::BuiltinDefineProperty) {
            vec![
                Arc::from("target"),
                Arc::from("Object"),
                Arc::from("defineProperty"),
                Arc::from("descriptor"),
            ]
        } else {
            vec![Arc::from("target")]
        },
        vec![
            template(0, entry_code, entry_map, entry_registers, 0),
            template(1, getter_code, getter_map, getter_registers, 0),
            template(2, method_code, method_map, method_registers, 1),
            template(
                3,
                descriptor_getter_code,
                descriptor_getter_map,
                descriptor_getter_registers,
                0,
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}
