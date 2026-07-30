use super::{fixtures::test_isolate, *};

#[test]
fn accessor_getter_uses_original_receiver_for_every_dispatch_batch() {
    assert_accessor_getter_batch::<1>();
    assert_accessor_getter_batch::<2>();
    assert_accessor_getter_batch::<4>();
    assert_accessor_getter_batch::<8>();
    assert_accessor_getter_batch::<16>();
}

#[test]
fn accessor_setter_preserves_receiver_rhs_and_order_for_every_dispatch_batch() {
    assert_accessor_setter_batch::<1>();
    assert_accessor_setter_batch::<2>();
    assert_accessor_setter_batch::<4>();
    assert_accessor_setter_batch::<8>();
    assert_accessor_setter_batch::<16>();
}

#[test]
fn accessor_getter_throw_reaches_caller_catch_for_every_dispatch_batch() {
    assert_accessor_throw_batch::<1>();
    assert_accessor_throw_batch::<2>();
    assert_accessor_throw_batch::<4>();
    assert_accessor_throw_batch::<8>();
    assert_accessor_throw_batch::<16>();
}

#[test]
fn missing_setter_obeys_sloppy_and_strict_assignment_boundaries() {
    assert_missing_setter_batch::<1>();
    assert_missing_setter_batch::<2>();
    assert_missing_setter_batch::<4>();
    assert_missing_setter_batch::<8>();
    assert_missing_setter_batch::<16>();
}

#[test]
fn accessor_setter_chains_accessor_valued_conversion_for_every_dispatch_batch() {
    assert_nested_accessor_conversion_batch::<1>();
    assert_nested_accessor_conversion_batch::<2>();
    assert_nested_accessor_conversion_batch::<4>();
    assert_nested_accessor_conversion_batch::<8>();
    assert_nested_accessor_conversion_batch::<16>();
}

#[test]
fn compound_assignment_observes_getter_rhs_setter_order_for_every_dispatch_batch() {
    assert_compound_accessor_order_batch::<1>();
    assert_compound_accessor_order_batch::<2>();
    assert_compound_accessor_order_batch::<4>();
    assert_compound_accessor_order_batch::<8>();
    assert_compound_accessor_order_batch::<16>();
}

#[test]
fn literal_accessor_definition_opcodes_merge_pairs_for_every_dispatch_batch() {
    assert_literal_accessor_definition_batch::<1>();
    assert_literal_accessor_definition_batch::<2>();
    assert_literal_accessor_definition_batch::<4>();
    assert_literal_accessor_definition_batch::<8>();
    assert_literal_accessor_definition_batch::<16>();
}

#[test]
fn computed_accessor_definition_and_names_work_for_every_dispatch_batch() {
    assert_computed_accessor_batch::<1>();
    assert_computed_accessor_batch::<2>();
    assert_computed_accessor_batch::<4>();
    assert_computed_accessor_batch::<8>();
    assert_computed_accessor_batch::<16>();
}

/// Exercises the runtime-key accessor opcodes and their function-name allocation in every batch.
fn assert_computed_accessor_batch<const N: usize>() {
    let module = computed_accessor_definition_module();
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 64,
                quantum: 64,
            },
        )
        .unwrap();
    assert_eq!(outcome, RunOutcome::Completed(Value::from_i32(42)));
}

/// Runs the compiler-facing getter/setter definition opcode sequence through every dispatch batch.
fn assert_literal_accessor_definition_batch<const N: usize>() {
    let module = literal_accessor_definition_module();
    let mut isolate = test_isolate();
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
    assert_eq!(outcome, RunOutcome::Completed(Value::from_i32(42)));
}

/// Builds a getter/setter pair as emitted for `{ get value() {}, set value(_) {} }`.
fn literal_accessor_definition_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::default();
    entry.emit(Opcode::CreateObject, &[0], span).unwrap();
    entry.emit(Opcode::CreateClosure, &[1, 1], span).unwrap();
    entry
        .emit(Opcode::DefineGetterById, &[0, 1, 0], span)
        .unwrap();
    entry.emit(Opcode::CreateClosure, &[2, 2], span).unwrap();
    entry
        .emit(Opcode::DefineSetterById, &[0, 2, 0], span)
        .unwrap();
    entry.emit(Opcode::LoadImmediate, &[3, 42], span).unwrap();
    entry.emit(Opcode::SetById, &[0, 3, 0], span).unwrap();
    entry.emit(Opcode::GetById, &[4, 0, 0], span).unwrap();
    entry.emit(Opcode::Return, &[4], span).unwrap();
    let (entry_bytecode, entry_map, entry_registers) = entry.finish().unwrap();

    let mut getter = BytecodeBuilder::default();
    getter.emit(Opcode::LoadThis, &[0], span).unwrap();
    getter.emit(Opcode::GetById, &[1, 0, 1], span).unwrap();
    getter.emit(Opcode::Return, &[1], span).unwrap();
    let (getter_bytecode, getter_map, getter_registers) = getter.finish().unwrap();

    let mut setter = BytecodeBuilder::default();
    setter.emit(Opcode::LoadThis, &[1], span).unwrap();
    setter.emit(Opcode::SetById, &[1, 0, 1], span).unwrap();
    setter.emit(Opcode::ReturnUndefined, &[], span).unwrap();
    let (setter_bytecode, setter_map, setter_registers) = setter.finish().unwrap();

    CompiledModule::new(
        Arc::from("literal accessor definition"),
        Vec::new(),
        vec![Arc::from("value"), Arc::from("seen")],
        vec![
            CompiledFunctionTemplate::new(
                FunctionId::new(0),
                entry_bytecode,
                FunctionMetadata {
                    layout: FunctionLayout {
                        register_count: entry_registers,
                        ..FunctionLayout::default()
                    },
                    source_map: entry_map,
                    ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
                },
            ),
            CompiledFunctionTemplate::new(
                FunctionId::new(1),
                getter_bytecode,
                FunctionMetadata {
                    layout: FunctionLayout {
                        register_count: getter_registers,
                        ..FunctionLayout::default()
                    },
                    source_map: getter_map,
                    ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
                },
            ),
            CompiledFunctionTemplate::new(
                FunctionId::new(2),
                setter_bytecode,
                FunctionMetadata {
                    layout: FunctionLayout {
                        register_count: setter_registers,
                        argument_count: 1,
                        ..FunctionLayout::default()
                    },
                    source_map: setter_map,
                    ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
                },
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds a dynamic-key accessor whose name allocation and definition share one opcode path.
fn computed_accessor_definition_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::default();
    entry.emit(Opcode::CreateObject, &[0], span).unwrap();
    entry.emit(Opcode::LoadImmediate, &[1, 1], span).unwrap();
    entry.emit(Opcode::CreateClosure, &[2, 1], span).unwrap();
    entry
        .emit(Opcode::SetAccessorFunctionName, &[2, 1, 1], span)
        .unwrap();
    entry
        .emit(Opcode::DefineGetterByValue, &[0, 2, 1], span)
        .unwrap();
    entry.emit(Opcode::GetByValue, &[3, 0, 1], span).unwrap();
    entry.emit(Opcode::Return, &[3], span).unwrap();
    let (entry_bytecode, entry_map, entry_registers) = entry.finish().unwrap();

    let mut getter = BytecodeBuilder::default();
    getter.emit(Opcode::LoadImmediate, &[0, 42], span).unwrap();
    getter.emit(Opcode::Return, &[0], span).unwrap();
    let (getter_bytecode, getter_map, getter_registers) = getter.finish().unwrap();

    CompiledModule::new(
        Arc::from("computed accessor definition"),
        Vec::new(),
        Vec::new(),
        vec![
            CompiledFunctionTemplate::new(
                FunctionId::new(0),
                entry_bytecode,
                FunctionMetadata {
                    layout: FunctionLayout {
                        register_count: entry_registers,
                        ..FunctionLayout::default()
                    },
                    source_map: entry_map,
                    ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
                },
            ),
            CompiledFunctionTemplate::new(
                FunctionId::new(1),
                getter_bytecode,
                FunctionMetadata {
                    layout: FunctionLayout {
                        register_count: getter_registers,
                        ..FunctionLayout::default()
                    },
                    source_map: getter_map,
                    ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
                },
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

#[test]
fn property_descriptor_getters_survive_forced_major_for_every_dispatch_batch() {
    assert_property_descriptor_batches::<1>();
    assert_property_descriptor_batches::<2>();
    assert_property_descriptor_batches::<4>();
    assert_property_descriptor_batches::<8>();
    assert_property_descriptor_batches::<16>();
}

#[derive(Clone, Copy)]
enum DescriptorGetterResult {
    Truthy,
    Object,
    Callable,
}

/// Exercises every ToPropertyDescriptor field after a bytecode getter suspension and forced GC.
fn assert_property_descriptor_batches<const N: usize>() {
    for (field, result) in [
        (b"enumerable".as_slice(), DescriptorGetterResult::Truthy),
        (b"configurable".as_slice(), DescriptorGetterResult::Truthy),
        (b"value".as_slice(), DescriptorGetterResult::Object),
        (b"writable".as_slice(), DescriptorGetterResult::Truthy),
        (b"get".as_slice(), DescriptorGetterResult::Callable),
        (b"set".as_slice(), DescriptorGetterResult::Callable),
    ] {
        for inherited in [false, true] {
            let module = property_descriptor_getter_module(result);
            let mut isolate = test_isolate();
            let target =
                install_property_descriptor_getter(&mut isolate, &module, field, inherited);
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
            assert_eq!(outcome, RunOutcome::Completed(target));
            let key = isolate.intern_intrinsic_name(b"result").unwrap();
            let descriptor = isolate
                .complete_own_property_descriptor(target, key)
                .unwrap()
                .expect("defineProperty publishes the requested descriptor");
            match (field, descriptor) {
                (b"get" | b"set", PropertyDescriptor::Accessor(_)) => {}
                (_, PropertyDescriptor::Data(_)) => {}
                _ => panic!("getter field selected the wrong descriptor kind"),
            }
        }
    }
}

/// Publishes one own or inherited descriptor-field getter and all call arguments as globals.
fn install_property_descriptor_getter(
    isolate: &mut Isolate,
    module: &CompiledModule,
    field: &[u8],
    inherited: bool,
) -> Value {
    let code = isolate.load_module(module).unwrap();
    let callback = allocate_bytecode_test_function(isolate, code, FunctionId::new(1));
    let owner = isolate.create_ordinary_object().unwrap();
    let descriptor = if inherited {
        isolate
            .create_ordinary_object_with_prototype(owner)
            .unwrap()
    } else {
        owner
    };
    let field = isolate.intern_intrinsic_name(field).unwrap();
    isolate
        .define_property(
            owner,
            field.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(callback),
                setter: None,
                enumerable: Some(true),
                configurable: Some(true),
            }),
        )
        .unwrap();
    let target = isolate.create_ordinary_object().unwrap();
    let key_atom = isolate.intern_intrinsic_name(b"result").unwrap();
    let key = isolate.atom_string_value(key_atom).unwrap();
    let values = [
        (
            b"define".as_slice(),
            isolate.realm.object_define_property.unwrap(),
        ),
        (b"target".as_slice(), target),
        (b"key".as_slice(), key),
        (b"descriptor".as_slice(), descriptor),
        (
            b"callable".as_slice(),
            isolate.realm.object_constructor.unwrap(),
        ),
    ];
    for (name, value) in values {
        let atom = isolate.intern_intrinsic_name(name).unwrap();
        isolate.realm.set(atom, value).unwrap();
    }
    target
}

/// Builds one defineProperty call and a getter that allocates before returning its field value.
fn property_descriptor_getter_module(result: DescriptorGetterResult) -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::default();
    for (register, scope) in (0..4).zip(0..4) {
        entry
            .emit(Opcode::LoadScope, &[register, scope], span)
            .unwrap();
    }
    entry.emit(Opcode::Call, &[4, 0, 3], span).unwrap();
    entry.emit(Opcode::Return, &[4], span).unwrap();
    let entry = entry.finish().unwrap();

    let mut getter = BytecodeBuilder::default();
    getter.emit(Opcode::CreateObject, &[0], span).unwrap();
    match result {
        DescriptorGetterResult::Truthy => {
            getter.emit(Opcode::LoadImmediate, &[1, 1], span).unwrap();
            getter.emit(Opcode::Return, &[1], span).unwrap();
        }
        DescriptorGetterResult::Object => {
            getter.emit(Opcode::Return, &[0], span).unwrap();
        }
        DescriptorGetterResult::Callable => {
            getter.emit(Opcode::LoadScope, &[1, 4], span).unwrap();
            getter.emit(Opcode::Return, &[1], span).unwrap();
        }
    }
    let getter = getter.finish().unwrap();
    accessor_test_module(
        "resumable property descriptor getter",
        vec![
            Arc::from("define"),
            Arc::from("target"),
            Arc::from("key"),
            Arc::from("descriptor"),
            Arc::from("callable"),
        ],
        entry,
        Some((getter.0, getter.1, getter.2, 0)),
        FunctionStrictness::Sloppy,
        Arc::default(),
    )
}

/// Runs own and inherited getter callbacks through one forced-major suspension.
fn assert_accessor_getter_batch<const N: usize>() {
    for inherited in [false, true] {
        let module = accessor_getter_module();
        let mut isolate = test_isolate();
        let target = install_bytecode_accessor(&mut isolate, &module, true, inherited);
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute_with_batch::<N>(
                &module,
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        assert_eq!(outcome, RunOutcome::Completed(target));
    }
}

/// Proves setter `this`, argument copying, caller-register restoration, and post-call ordering.
fn assert_accessor_setter_batch<const N: usize>() {
    for inherited in [false, true] {
        let module = accessor_setter_module();
        let mut isolate = test_isolate();
        install_bytecode_accessor(&mut isolate, &module, false, inherited);
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

/// Ensures a throw from the getter frame is matched at the original property instruction.
fn assert_accessor_throw_batch<const N: usize>() {
    let module = accessor_throw_module();
    let mut isolate = test_isolate();
    install_bytecode_accessor(&mut isolate, &module, true, false);
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 32,
                quantum: 32,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(7)));
}

/// Locks accessor [[Set]] false to sloppy no-op and strict TypeError behavior.
fn assert_missing_setter_batch<const N: usize>() {
    for strictness in [FunctionStrictness::Sloppy, FunctionStrictness::Strict] {
        let module = missing_setter_module(strictness);
        let mut isolate = test_isolate();
        install_missing_setter(&mut isolate, &module);
        let outcome = isolate
            .execute_with_batch::<N>(
                &module,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        if strictness == FunctionStrictness::Sloppy {
            assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
            continue;
        }
        let RunOutcome::Thrown(error) = outcome else {
            panic!("strict missing setter must throw");
        };
        let (_, snapshot) = isolate.object_snapshot(error).unwrap();
        assert_eq!(
            snapshot.prototype,
            isolate
                .realm
                .error_intrinsics
                .get(NativeErrorKind::Type)
                .prototype
                .unwrap()
        );
    }
}

/// Stacks PropertySet, conversion-getter, and conversion-method continuations in one assignment.
fn assert_nested_accessor_conversion_batch<const N: usize>() {
    let module = nested_accessor_conversion_module();
    let mut isolate = test_isolate();
    let code = isolate.load_module(&module).unwrap();
    let getter = allocate_bytecode_test_function(&mut isolate, code, FunctionId::new(1));
    let method = allocate_bytecode_test_function(&mut isolate, code, FunctionId::new(2));
    let target = isolate.create_ordinary_object().unwrap();
    let rhs = isolate.create_ordinary_object().unwrap();
    for (name, value) in [
        (b"target".as_slice(), target),
        (b"rhs", rhs),
        (b"method", method),
    ] {
        let atom = isolate.intern_intrinsic_name(name).unwrap();
        isolate.realm.set(atom, value).unwrap();
    }
    let value_of = isolate.intern_intrinsic_name(b"valueOf").unwrap();
    isolate
        .define_property(
            rhs,
            value_of.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(getter),
                setter: None,
                enumerable: true.into(),
                configurable: true.into(),
            }),
        )
        .unwrap();
    let key = isolate.intern_intrinsic_name(b"x").unwrap();
    isolate
        .define_property(
            target,
            key.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: None,
                setter: isolate.realm.number_constructor,
                enumerable: true.into(),
                configurable: true.into(),
            }),
        )
        .unwrap();
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
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

/// Makes getter, RHS, and setter publish values that encode their exact observable order.
fn assert_compound_accessor_order_batch<const N: usize>() {
    let module = compound_accessor_order_module();
    let mut isolate = test_isolate();
    let code = isolate.load_module(&module).unwrap();
    let getter = allocate_bytecode_test_function(&mut isolate, code, FunctionId::new(1));
    let rhs = allocate_bytecode_test_function(&mut isolate, code, FunctionId::new(2));
    let setter = allocate_bytecode_test_function(&mut isolate, code, FunctionId::new(3));
    let target = isolate.create_ordinary_object().unwrap();
    for (name, value) in [(b"target".as_slice(), target), (b"rhs", rhs)] {
        let atom = isolate.intern_intrinsic_name(name).unwrap();
        isolate.realm.set(atom, value).unwrap();
    }
    let key = isolate.intern_intrinsic_name(b"x").unwrap();
    isolate
        .define_property(
            target,
            key.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(getter),
                setter: Some(setter),
                enumerable: true.into(),
                configurable: true.into(),
            }),
        )
        .unwrap();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 64,
                quantum: 64,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(13)));
}

/// Installs function 1 as one accessor and publishes the target through a global binding.
pub(super) fn install_bytecode_accessor(
    isolate: &mut Isolate,
    module: &CompiledModule,
    getter: bool,
    inherited: bool,
) -> Value {
    let code = isolate.load_module(module).unwrap();
    let callback = allocate_bytecode_test_function(isolate, code, FunctionId::new(1));
    let owner = isolate.create_ordinary_object().unwrap();
    let target = if inherited {
        isolate
            .create_ordinary_object_with_prototype(owner)
            .unwrap()
    } else {
        owner
    };
    let target_atom = isolate.intern_intrinsic_name(b"target").unwrap();
    isolate.realm.set(target_atom, target).unwrap();
    let key = isolate.intern_intrinsic_name(b"x").unwrap();
    let descriptor = if getter {
        AccessorPropertyDescriptor {
            getter: Some(callback),
            setter: None,
            enumerable: Some(true),
            configurable: Some(true),
        }
    } else {
        AccessorPropertyDescriptor {
            getter: None,
            setter: Some(callback),
            enumerable: Some(true),
            configurable: Some(true),
        }
    };
    isolate
        .define_property(owner, key.into(), PropertyDescriptor::Accessor(descriptor))
        .unwrap();
    target
}

/// Builds getter throw -> finally replay -> outer catch without native recursion.
pub(super) fn accessor_finally_throw_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::default();
    entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    let outer_start = entry.emit(Opcode::Nop, &[], span).unwrap();
    let inner_start = entry.emit(Opcode::GetById, &[1, 0, 1], span).unwrap();
    entry.emit(Opcode::EnterFinally, &[], span).unwrap();
    let finalizer = entry.emit(Opcode::LoadImmediate, &[2, 1], span).unwrap();
    entry.emit(Opcode::ResumeCompletion, &[], span).unwrap();
    let finalizer_end = entry.current_offset().unwrap();
    let catch = entry.emit(Opcode::LoadException, &[3], span).unwrap();
    entry.emit(Opcode::Add, &[4, 2, 3], span).unwrap();
    entry.emit(Opcode::Return, &[4], span).unwrap();
    let (entry_bytecode, entry_map, entry_registers) = entry.finish().unwrap();
    let mut getter = BytecodeBuilder::default();
    getter.emit(Opcode::LoadImmediate, &[0, 4], span).unwrap();
    getter.emit(Opcode::Throw, &[0], span).unwrap();
    let (getter_bytecode, getter_map, getter_registers) = getter.finish().unwrap();
    let handlers = vec![
        HandlerEntry {
            protected_start: outer_start,
            protected_end: catch,
            handler: catch,
            handler_end: catch,
            kind: HandlerKind::Catch,
            environment_depth: 0,
        },
        HandlerEntry {
            protected_start: inner_start,
            protected_end: finalizer,
            handler: finalizer,
            handler_end: finalizer_end,
            kind: HandlerKind::Finally,
            environment_depth: 0,
        },
    ]
    .into();
    accessor_test_module(
        "accessor getter throw through finally",
        vec![Arc::from("target"), Arc::from("x")],
        (entry_bytecode, entry_map, entry_registers),
        Some((getter_bytecode, getter_map, getter_registers, 0)),
        FunctionStrictness::Sloppy,
        handlers,
    )
}

/// Publishes an accessor with neither callback for strict/sloppy assignment tests.
fn install_missing_setter(isolate: &mut Isolate, module: &CompiledModule) {
    isolate.load_module(module).unwrap();
    let target = isolate.create_ordinary_object().unwrap();
    let target_atom = isolate.intern_intrinsic_name(b"target").unwrap();
    isolate.realm.set(target_atom, target).unwrap();
    let key = isolate.intern_intrinsic_name(b"x").unwrap();
    isolate
        .define_property(
            target,
            key.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: None,
                setter: None,
                enumerable: Some(true),
                configurable: Some(true),
            }),
        )
        .unwrap();
}

/// Allocates one bytecode closure without entering its function on the Rust stack.
fn allocate_bytecode_test_function(
    isolate: &mut Isolate,
    code: CodeId,
    function: FunctionId,
) -> Value {
    let function_type = isolate.types.function;
    let prototype = isolate.realm.function_prototype.unwrap();
    let roots = &mut VmRoots {
        fiber: &mut isolate.fiber,
        suspended_fibers: &mut isolate.suspended_fibers,
        finalization_jobs: &mut isolate.finalization_jobs,
        promise_jobs: &mut isolate.promise_jobs,
        realm: &mut isolate.realm,
        inactive_realms: &mut isolate.inactive_realms,
        loaded_code: &mut isolate.loaded_code,
        module_graph: &mut isolate.module_graph,
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
                realm: RealmId::MAIN,
                prototype_or_home_object: FunctionAuxiliaryEdge::NONE,
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

/// Builds a getter that allocates under forced GC before returning its original receiver.
fn accessor_getter_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::default();
    entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    entry.emit(Opcode::GetById, &[1, 0, 1], span).unwrap();
    entry.emit(Opcode::Return, &[1], span).unwrap();
    let (entry_bytecode, entry_map, entry_registers) = entry.finish().unwrap();
    let mut getter = BytecodeBuilder::default();
    getter.emit(Opcode::CreateObject, &[0], span).unwrap();
    getter.emit(Opcode::LoadThis, &[1], span).unwrap();
    getter.emit(Opcode::Return, &[1], span).unwrap();
    let (getter_bytecode, getter_map, getter_registers) = getter.finish().unwrap();
    accessor_test_module(
        "accessor getter receiver",
        vec![Arc::from("target"), Arc::from("x")],
        (entry_bytecode, entry_map, entry_registers),
        Some((getter_bytecode, getter_map, getter_registers, 0)),
        FunctionStrictness::Sloppy,
        Arc::default(),
    )
}

/// Builds a setter that allocates, writes its argument through `this`, and returns ignored data.
fn accessor_setter_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::default();
    entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    entry.emit(Opcode::LoadImmediate, &[1, 42], span).unwrap();
    entry.emit(Opcode::SetById, &[0, 1, 1], span).unwrap();
    entry.emit(Opcode::GetById, &[2, 0, 2], span).unwrap();
    entry.emit(Opcode::StrictEqual, &[3, 1, 2], span).unwrap();
    entry.emit(Opcode::Return, &[3], span).unwrap();
    let (entry_bytecode, entry_map, entry_registers) = entry.finish().unwrap();
    let mut setter = BytecodeBuilder::default();
    setter.emit(Opcode::LoadThis, &[1], span).unwrap();
    setter.emit(Opcode::CreateObject, &[2], span).unwrap();
    setter.emit(Opcode::SetById, &[1, 0, 2], span).unwrap();
    setter.emit(Opcode::ReturnUndefined, &[], span).unwrap();
    let (setter_bytecode, setter_map, setter_registers) = setter.finish().unwrap();
    accessor_test_module(
        "accessor setter order",
        vec![Arc::from("target"), Arc::from("x"), Arc::from("seen")],
        (entry_bytecode, entry_map, entry_registers),
        Some((setter_bytecode, setter_map, setter_registers, 1)),
        FunctionStrictness::Sloppy,
        Arc::default(),
    )
}

/// Builds a caller catch range around a getter whose callback frame throws seven.
fn accessor_throw_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(10, 1);
    let end = entry.new_label().unwrap();
    entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    let protected_start = entry.emit(Opcode::Nop, &[], span).unwrap();
    entry.emit(Opcode::GetById, &[1, 0, 1], span).unwrap();
    entry.emit_jump(end, span).unwrap();
    let handler = entry.emit(Opcode::LoadException, &[2], span).unwrap();
    entry.emit(Opcode::Return, &[2], span).unwrap();
    entry.bind_label(end).unwrap();
    entry.emit(Opcode::ReturnUndefined, &[], span).unwrap();
    let (entry_bytecode, entry_map, entry_registers) = entry.finish().unwrap();
    let mut getter = BytecodeBuilder::default();
    getter.emit(Opcode::LoadImmediate, &[0, 7], span).unwrap();
    getter.emit(Opcode::Throw, &[0], span).unwrap();
    let (getter_bytecode, getter_map, getter_registers) = getter.finish().unwrap();
    let handlers = vec![HandlerEntry {
        protected_start,
        protected_end: handler,
        handler,
        handler_end: handler,
        kind: HandlerKind::Catch,
        environment_depth: 0,
    }]
    .into();
    accessor_test_module(
        "accessor getter throw",
        vec![Arc::from("target"), Arc::from("x")],
        (entry_bytecode, entry_map, entry_registers),
        Some((getter_bytecode, getter_map, getter_registers, 0)),
        FunctionStrictness::Sloppy,
        handlers,
    )
}

/// Builds one assignment-only script whose strictness controls a missing-setter result.
fn missing_setter_module(strictness: FunctionStrictness) -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::default();
    entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    entry.emit(Opcode::LoadImmediate, &[1, 42], span).unwrap();
    entry.emit(Opcode::SetById, &[0, 1, 1], span).unwrap();
    entry.emit(Opcode::Return, &[1], span).unwrap();
    let (entry_bytecode, entry_map, entry_registers) = entry.finish().unwrap();
    accessor_test_module(
        "missing accessor setter",
        vec![Arc::from("target"), Arc::from("x")],
        (entry_bytecode, entry_map, entry_registers),
        None,
        strictness,
        Arc::default(),
    )
}

/// Builds PropertySet -> Number conversion -> accessor getter -> valueOf method nesting.
fn nested_accessor_conversion_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::default();
    entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    entry.emit(Opcode::LoadScope, &[1, 1], span).unwrap();
    entry.emit(Opcode::SetById, &[0, 1, 2], span).unwrap();
    entry.emit(Opcode::GetById, &[2, 1, 3], span).unwrap();
    entry.emit(Opcode::LoadImmediate, &[3, 9], span).unwrap();
    entry.emit(Opcode::StrictEqual, &[4, 2, 3], span).unwrap();
    entry.emit(Opcode::Return, &[4], span).unwrap();
    let (entry_bytecode, entry_map, entry_registers) = entry.finish().unwrap();

    let mut getter = BytecodeBuilder::default();
    getter.emit(Opcode::CreateObject, &[0], span).unwrap();
    getter.emit(Opcode::LoadScope, &[1, 4], span).unwrap();
    getter.emit(Opcode::Return, &[1], span).unwrap();
    let (getter_bytecode, getter_map, getter_registers) = getter.finish().unwrap();

    let mut method = BytecodeBuilder::default();
    method.emit(Opcode::LoadThis, &[0], span).unwrap();
    method.emit(Opcode::CreateObject, &[1], span).unwrap();
    method.emit(Opcode::LoadImmediate, &[2, 9], span).unwrap();
    method.emit(Opcode::SetById, &[0, 2, 3], span).unwrap();
    method.emit(Opcode::Return, &[2], span).unwrap();
    let (method_bytecode, method_map, method_registers) = method.finish().unwrap();

    let function = |id, bytecode, source_map, register_count, kind| {
        CompiledFunctionTemplate::new(
            FunctionId::new(id),
            bytecode,
            FunctionMetadata {
                layout: FunctionLayout {
                    register_count,
                    ..FunctionLayout::default()
                },
                source_map,
                ..FunctionMetadata::new(kind, FunctionLayout::default())
            },
        )
    };
    CompiledModule::new(
        Arc::from("nested accessor conversion"),
        Vec::new(),
        vec![
            Arc::from("target"),
            Arc::from("rhs"),
            Arc::from("x"),
            Arc::from("seen"),
            Arc::from("method"),
            Arc::from("valueOf"),
        ],
        vec![
            function(
                0,
                entry_bytecode,
                entry_map,
                entry_registers,
                FunctionKind::Script,
            ),
            function(
                1,
                getter_bytecode,
                getter_map,
                getter_registers,
                FunctionKind::Ordinary,
            ),
            function(
                2,
                method_bytecode,
                method_map,
                method_registers,
                FunctionKind::Ordinary,
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds the callback sequence equivalent to `target.x += rhs()` with ordered writes.
fn compound_accessor_order_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::default();
    entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    entry.emit(Opcode::GetById, &[1, 0, 2], span).unwrap();
    entry.emit(Opcode::LoadScope, &[2, 1], span).unwrap();
    entry.emit(Opcode::Call, &[3, 2, 0], span).unwrap();
    entry.emit(Opcode::Add, &[4, 1, 3], span).unwrap();
    entry.emit(Opcode::SetById, &[0, 4, 2], span).unwrap();
    entry.emit(Opcode::GetById, &[5, 0, 4], span).unwrap();
    entry.emit(Opcode::Return, &[5], span).unwrap();
    let (entry_bytecode, entry_map, entry_registers) = entry.finish().unwrap();

    let mut getter = BytecodeBuilder::default();
    getter.emit(Opcode::LoadThis, &[0], span).unwrap();
    getter.emit(Opcode::LoadImmediate, &[1, 1], span).unwrap();
    getter.emit(Opcode::SetById, &[0, 1, 3], span).unwrap();
    getter.emit(Opcode::LoadImmediate, &[2, 10], span).unwrap();
    getter.emit(Opcode::Return, &[2], span).unwrap();
    let (getter_bytecode, getter_map, getter_registers) = getter.finish().unwrap();

    let mut rhs = BytecodeBuilder::default();
    rhs.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    rhs.emit(Opcode::GetById, &[1, 0, 3], span).unwrap();
    rhs.emit(Opcode::LoadImmediate, &[2, 2], span).unwrap();
    rhs.emit(Opcode::SetById, &[0, 2, 3], span).unwrap();
    rhs.emit(Opcode::Return, &[1], span).unwrap();
    let (rhs_bytecode, rhs_map, rhs_registers) = rhs.finish().unwrap();

    let mut setter = BytecodeBuilder::default();
    setter.emit(Opcode::LoadThis, &[1], span).unwrap();
    setter.emit(Opcode::GetById, &[2, 1, 3], span).unwrap();
    setter.emit(Opcode::Add, &[3, 0, 2], span).unwrap();
    setter.emit(Opcode::SetById, &[1, 3, 4], span).unwrap();
    setter.emit(Opcode::ReturnUndefined, &[], span).unwrap();
    let (setter_bytecode, setter_map, setter_registers) = setter.finish().unwrap();

    let function = |id, bytecode, source_map, register_count, kind, argument_count| {
        CompiledFunctionTemplate::new(
            FunctionId::new(id),
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
    };
    CompiledModule::new(
        Arc::from("compound accessor order"),
        Vec::new(),
        vec![
            Arc::from("target"),
            Arc::from("rhs"),
            Arc::from("x"),
            Arc::from("order"),
            Arc::from("result"),
        ],
        vec![
            function(
                0,
                entry_bytecode,
                entry_map,
                entry_registers,
                FunctionKind::Script,
                0,
            ),
            function(
                1,
                getter_bytecode,
                getter_map,
                getter_registers,
                FunctionKind::Ordinary,
                0,
            ),
            function(
                2,
                rhs_bytecode,
                rhs_map,
                rhs_registers,
                FunctionKind::Ordinary,
                0,
            ),
            function(
                3,
                setter_bytecode,
                setter_map,
                setter_registers,
                FunctionKind::Ordinary,
                1,
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Freezes one entry and optional callback with exact handler and argument metadata.
fn accessor_test_module(
    source: &'static str,
    scope_names: Vec<Arc<str>>,
    entry: (Bytecode, Arc<[SourceMapEntry]>, u32),
    callback: Option<(Bytecode, Arc<[SourceMapEntry]>, u32, u32)>,
    strictness: FunctionStrictness,
    handlers: Arc<[HandlerEntry]>,
) -> CompiledModule {
    let (bytecode, source_map, register_count) = entry;
    let mut entry_metadata = FunctionMetadata {
        strictness,
        layout: FunctionLayout {
            register_count,
            max_handler_depth: handlers.len() as u32,
            max_completion_depth: handlers
                .iter()
                .filter(|handler| handler.kind == HandlerKind::Finally)
                .count() as u32,
            ..FunctionLayout::default()
        },
        source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    entry_metadata.handlers = handlers;
    let mut functions = vec![CompiledFunctionTemplate::new(
        FunctionId::new(0),
        bytecode,
        entry_metadata,
    )];
    if let Some((bytecode, source_map, register_count, argument_count)) = callback {
        functions.push(CompiledFunctionTemplate::new(
            FunctionId::new(1),
            bytecode,
            FunctionMetadata {
                layout: FunctionLayout {
                    register_count,
                    argument_count,
                    ..FunctionLayout::default()
                },
                source_map,
                ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
            },
        ));
    }
    CompiledModule::new(
        Arc::from(source),
        Vec::new(),
        scope_names,
        functions,
        FunctionId::new(0),
    )
    .unwrap()
}
