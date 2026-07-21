use super::{fixtures::*, *};

/// Builds a module activation that exercises the state-bearing environment storage through opcodes.
fn module_environment_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(4, 0);
    builder.emit(Opcode::LoadImmediate, &[0, 7], span).unwrap();
    builder
        .emit(Opcode::StoreEnvironment, &[0, 0, 0], span)
        .unwrap();
    builder
        .emit(Opcode::LoadEnvironment, &[1, 0, 0], span)
        .unwrap();
    builder.emit(Opcode::Return, &[1], span).unwrap();
    let (bytecode, source_map, register_count) = builder.finish().unwrap();
    let layout = FunctionLayout {
        register_count,
        environment_slot_count: 1,
        ..FunctionLayout::default()
    };
    CompiledModule::new(
        Arc::from("module environment"),
        vec![],
        vec![],
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            bytecode,
            FunctionMetadata {
                source_map,
                environment_slots: Arc::from([EnvironmentSlotMetadata {
                    name: Arc::from("value"),
                    mutable: true,
                    initialized: true,
                }]),
                ..FunctionMetadata::new(FunctionKind::Module, layout)
            },
        )],
        FunctionId::new(0),
    )
    .unwrap()
}

fn assert_module_environment_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &module_environment_module(),
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(7)));
}

/// Publishes one environment with its pending values rooted during pre-allocation collection.
fn allocate_managed_environment(
    isolate: &mut Isolate,
    environment: Environment,
) -> GcRef<Environment> {
    let roots = &mut VmRoots {
        fiber: &mut isolate.fiber,
        finalization_jobs: &mut isolate.finalization_jobs,
        promise_jobs: &mut isolate.promise_jobs,
        realm: &mut isolate.realm,
        loaded_code: &mut isolate.loaded_code,
    };
    isolate
        .heap
        .try_allocate_external_with_gc(
            isolate.types.environment,
            0,
            environment,
            AllocationSpace::Young,
            roots,
        )
        .unwrap()
}

#[test]
/// Checks all record categories without changing their current activation ownership rules.
fn environment_record_kinds_and_storage_accounting_are_explicit() {
    let one = NonZeroU32::new(1).unwrap();
    let global = Environment::try_captured(EnvironmentKind::Global, None, one).unwrap();
    let function = Environment::try_captured(EnvironmentKind::Function, None, one).unwrap();
    let declarative = Environment::try_bindings(EnvironmentKind::Declarative, None, one, |_| {
        BindingState::new(true, false)
    })
    .unwrap();
    let module = Environment::try_bindings(EnvironmentKind::Module, None, one, |_| {
        BindingState::new(false, false)
    })
    .unwrap();

    assert_eq!(global.kind(), EnvironmentKind::Global);
    assert_eq!(function.kind(), EnvironmentKind::Function);
    assert_eq!(declarative.kind(), EnvironmentKind::Declarative);
    assert_eq!(module.kind(), EnvironmentKind::Module);
    assert_eq!(global.external_memory_bytes(), size_of::<Value>());
    assert_eq!(function.external_memory_bytes(), size_of::<Value>());
    assert_eq!(
        declarative.external_memory_bytes(),
        size_of::<Value>() + size_of::<BindingState>()
    );
    assert_eq!(
        module.external_memory_bytes(),
        size_of::<Value>() + size_of::<BindingState>()
    );
    assert_eq!(
        EnvironmentKind::for_activation(FunctionKind::Script, false),
        EnvironmentKind::Global
    );
    assert_eq!(
        EnvironmentKind::for_activation(FunctionKind::Script, true),
        EnvironmentKind::Declarative
    );
}

#[test]
fn environment_access_errors_preserve_the_direct_slot_address() {
    assert_eq!(
        crate::interpreter::environment_access_error(2, 3, EnvironmentAccessError::Uninitialized),
        ExecutionError::UninitializedEnvironmentBinding { depth: 2, slot: 3 }
    );
    assert_eq!(
        crate::interpreter::environment_access_error(4, 5, EnvironmentAccessError::Immutable),
        ExecutionError::ImmutableEnvironmentBinding { depth: 4, slot: 5 }
    );
    assert_eq!(
        crate::interpreter::environment_access_error(
            6,
            7,
            EnvironmentAccessError::AlreadyInitialized
        ),
        ExecutionError::EnvironmentBindingAlreadyInitialized { depth: 6, slot: 7 }
    );
}

#[test]
/// Exercises initialization separately from assignment so TDZ and const cannot collapse together.
fn binding_storage_enforces_tdz_mutability_and_single_initialization() {
    let states = [
        BindingState::new(true, false),
        BindingState::new(false, false),
        BindingState::new(true, true),
    ];
    let mut environment = Environment::try_bindings(
        EnvironmentKind::Declarative,
        None,
        NonZeroU32::new(states.len() as u32).unwrap(),
        |slot| states[slot as usize],
    )
    .unwrap();

    assert_eq!(
        environment.load(0),
        Err(EnvironmentAccessError::Uninitialized)
    );
    assert_eq!(
        environment.store(0, Value::from_i32(1)),
        Err(EnvironmentAccessError::Uninitialized)
    );
    environment.initialize(0, Value::from_i32(1)).unwrap();
    assert_eq!(environment.load(0).unwrap().as_i32(), Some(1));
    environment.store(0, Value::from_i32(2)).unwrap();
    assert_eq!(environment.load(0).unwrap().as_i32(), Some(2));
    assert_eq!(
        environment.initialize(0, Value::from_i32(3)),
        Err(EnvironmentAccessError::AlreadyInitialized)
    );

    environment.initialize(1, Value::from_i32(4)).unwrap();
    assert_eq!(environment.load(1).unwrap().as_i32(), Some(4));
    assert_eq!(
        environment.store(1, Value::from_i32(5)),
        Err(EnvironmentAccessError::Immutable)
    );
    assert_eq!(
        environment.initialize(3, Value::from_i32(6)),
        Err(EnvironmentAccessError::InvalidSlot)
    );
}

#[test]
fn state_bearing_environment_opcodes_work_for_every_dispatch_batch() {
    assert_module_environment_batch::<1>();
    assert_module_environment_batch::<2>();
    assert_module_environment_batch::<4>();
    assert_module_environment_batch::<8>();
    assert_module_environment_batch::<16>();
}

#[test]
/// A forced major must retain both the binding state bytes and a managed value reached through them.
fn binding_environment_state_and_value_survive_forced_major() {
    let mut isolate = test_isolate();
    let module = state_module(
        FunctionKind::Script,
        FunctionLayout {
            register_count: 1,
            ..FunctionLayout::default()
        },
    );
    let code = isolate.load_module(&module).unwrap();
    isolate.enter(code, FunctionId::new(0)).unwrap();
    let string = isolate
        .allocate_runtime_string(JsString::try_from_str("environment-root").unwrap())
        .unwrap();
    let mut environment = Environment::try_bindings(
        EnvironmentKind::Declarative,
        None,
        NonZeroU32::new(1).unwrap(),
        |_| BindingState::new(false, false),
    )
    .unwrap();
    environment.initialize(0, string).unwrap();

    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let environment = allocate_managed_environment(&mut isolate, environment);
    isolate
        .fiber
        .frames
        .last_mut()
        .expect("entry frame remains active")
        .environment = Some(environment);
    isolate.create_ordinary_object().unwrap();

    let environment = isolate
        .fiber
        .frames
        .last()
        .and_then(|frame| frame.environment)
        .expect("frame roots the environment");
    let environment_type = isolate.types.environment;
    let (kind, value, immutable) = isolate.heap.with_running_scope(|scope| {
        scope.with_no_gc_scope(|no_gc| {
            let environment = no_gc
                .borrow_reference_mut(environment, environment_type)
                .unwrap();
            (
                environment.kind(),
                environment.load(0).unwrap(),
                environment.store(0, Value::from_i32(1)).unwrap_err(),
            )
        })
    });
    assert_eq!(kind, EnvironmentKind::Declarative);
    assert_eq!(immutable, EnvironmentAccessError::Immutable);
    assert!(isolate.is_string_value(value));
}
