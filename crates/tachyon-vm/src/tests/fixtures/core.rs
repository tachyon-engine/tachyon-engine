use super::super::*;
use super::*;

pub(in crate::tests) fn test_isolate() -> Isolate {
    Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(8 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("test isolate descriptors register")
}

pub(in crate::tests) fn arithmetic_module() -> CompiledModule {
    binary_module(Opcode::Add, "1 + 2")
}

pub(in crate::tests) fn less_than_module() -> CompiledModule {
    binary_module(Opcode::LessThan, "1 < 2")
}

/// Builds a closure whose empty activation inherits and mutates the entry environment.
pub(in crate::tests) fn captured_environment_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::default();
    entry.emit(Opcode::LoadImmediate, &[0, 1], span).unwrap();
    entry
        .emit(Opcode::StoreEnvironment, &[0, 0, 0], span)
        .unwrap();
    entry.emit(Opcode::CreateClosure, &[1, 1], span).unwrap();
    entry.emit(Opcode::Call, &[2, 1, 0], span).unwrap();
    entry.emit(Opcode::Call, &[3, 1, 0], span).unwrap();
    entry.emit(Opcode::Return, &[3], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

    let mut closure = BytecodeBuilder::default();
    closure
        .emit(Opcode::LoadEnvironment, &[0, 0, 0], span)
        .unwrap();
    closure.emit(Opcode::LoadImmediate, &[1, 1], span).unwrap();
    closure.emit(Opcode::Add, &[2, 0, 1], span).unwrap();
    closure
        .emit(Opcode::StoreEnvironment, &[2, 0, 0], span)
        .unwrap();
    closure.emit(Opcode::Return, &[2], span).unwrap();
    let (closure_bytecode, closure_source_map, closure_registers) = closure.finish().unwrap();
    let binding_plan: Arc<[BindingPlanEntry]> = Arc::from([BindingPlanEntry {
        name: Arc::from("value"),
        location: BindingLocation::Environment { depth: 0, slot: 0 },
        mutable: true,
    }]);
    CompiledModule::new(
        Arc::from("captured environment"),
        vec![],
        vec![],
        vec![
            CompiledFunctionTemplate::new(
                FunctionId::new(0),
                entry_bytecode,
                FunctionMetadata {
                    kind: FunctionKind::Script,
                    strictness: FunctionStrictness::Sloppy,
                    layout: FunctionLayout {
                        register_count: entry_registers,
                        environment_slot_count: 1,
                        ..FunctionLayout::default()
                    },
                    source_map: entry_source_map,
                    binding_plan: binding_plan.clone(),
                    ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
                },
            ),
            CompiledFunctionTemplate::new(
                FunctionId::new(1),
                closure_bytecode,
                FunctionMetadata {
                    kind: FunctionKind::Ordinary,
                    strictness: FunctionStrictness::Sloppy,
                    layout: FunctionLayout {
                        register_count: closure_registers,
                        ..FunctionLayout::default()
                    },
                    source_map: closure_source_map,
                    binding_plan,
                    ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
                },
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Compares the canonical typeof number value with an independently loaded string literal.
pub(in crate::tests) fn typeof_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(5, 0);
    builder.emit(Opcode::LoadImmediate, &[0, 1], span).unwrap();
    builder.emit(Opcode::Typeof, &[1, 0], span).unwrap();
    builder.emit(Opcode::LoadConstant, &[2, 0], span).unwrap();
    builder.emit(Opcode::StrictEqual, &[3, 1, 2], span).unwrap();
    builder.emit(Opcode::Return, &[3], span).unwrap();
    single_function_module(
        "typeof number",
        vec![BytecodeConstant::string_from_utf16(
            "number".encode_utf16().collect(),
        )],
        builder,
    )
}

/// Loads two distinct strings so forced collection runs while the pending cache owns a root.
pub(in crate::tests) fn string_constant_root_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(4, 0);
    builder.emit(Opcode::LoadConstant, &[0, 0], span).unwrap();
    builder.emit(Opcode::LoadConstant, &[1, 1], span).unwrap();
    builder.emit(Opcode::StrictEqual, &[2, 0, 1], span).unwrap();
    builder.emit(Opcode::Return, &[2], span).unwrap();
    single_function_module(
        "rooted strings",
        vec![
            BytecodeConstant::string_from_utf16("left".encode_utf16().collect()),
            BytecodeConstant::string_from_utf16("right".encode_utf16().collect()),
        ],
        builder,
    )
}

/// Declares one global twice around a write so dispatch tests prove redeclaration is a no-op.
pub(in crate::tests) fn scoped_var_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(6, 0);
    builder.emit(Opcode::DeclareScope, &[0], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[0, 7], span).unwrap();
    builder
        .emit(Opcode::StoreResolvedScope, &[0, 0], span)
        .unwrap();
    builder.emit(Opcode::DeclareScope, &[0], span).unwrap();
    builder.emit(Opcode::LoadScope, &[1, 0], span).unwrap();
    builder.emit(Opcode::Return, &[1], span).unwrap();
    let (bytecode, source_map, register_count) = builder.finish().unwrap();
    CompiledModule::new(
        Arc::from("var answer = 7; var answer; answer;"),
        Vec::new(),
        vec![Arc::from("answer")],
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            bytecode,
            FunctionMetadata {
                layout: FunctionLayout {
                    register_count,
                    ..FunctionLayout::default()
                },
                source_map,
                ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
            },
        )],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Exercises declarative global declaration, one-time initialization, and lexical-first load.
pub(in crate::tests) fn global_lexical_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(5, 0);
    builder
        .emit(Opcode::DeclareGlobalLexical, &[0, 1], span)
        .unwrap();
    builder.emit(Opcode::LoadImmediate, &[0, 42], span).unwrap();
    builder
        .emit(Opcode::InitializeGlobalLexical, &[0, 0], span)
        .unwrap();
    builder.emit(Opcode::LoadScope, &[1, 0], span).unwrap();
    builder.emit(Opcode::Return, &[1], span).unwrap();
    let (bytecode, source_map, register_count) = builder.finish().unwrap();
    CompiledModule::new(
        Arc::from("let answer = 42; answer;"),
        Vec::new(),
        vec![Arc::from("answer")],
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            bytecode,
            FunctionMetadata {
                layout: FunctionLayout {
                    register_count,
                    ..FunctionLayout::default()
                },
                source_map,
                ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
            },
        )],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Freezes one builder into a script module with caller-provided immutable constants.
pub(in crate::tests) fn single_function_module(
    source: &'static str,
    constants: Vec<BytecodeConstant>,
    builder: BytecodeBuilder,
) -> CompiledModule {
    let (bytecode, source_map, register_count) = builder.finish().unwrap();
    let metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count,
            ..FunctionLayout::default()
        },
        source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    CompiledModule::new(
        Arc::from(source),
        constants,
        Vec::new(),
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            bytecode,
            metadata,
        )],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds a minimal verified binary-op fixture over the integer values one and two.
pub(in crate::tests) fn binary_module(opcode: Opcode, source: &'static str) -> CompiledModule {
    let mut words = encode_instruction(Opcode::LoadImmediate, &[0, 1]).unwrap();
    words.extend(encode_instruction(Opcode::LoadImmediate, &[1, 2]).unwrap());
    words.extend(encode_instruction(opcode, &[2, 0, 1]).unwrap());
    words.extend(encode_instruction(Opcode::Return, &[2]).unwrap());
    let metadata = FunctionMetadata::new(
        FunctionKind::Script,
        FunctionLayout {
            register_count: 3,
            ..FunctionLayout::default()
        },
    );
    CompiledModule::new(
        Arc::from(source),
        Vec::new(),
        Vec::new(),
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            Bytecode::from_words(words),
            metadata,
        )],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds a call whose integer callee must become a language-level TypeError.
pub(in crate::tests) fn non_callable_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::default();
    builder.emit(Opcode::LoadImmediate, &[0, 1], span).unwrap();
    builder.emit(Opcode::Call, &[1, 0, 0], span).unwrap();
    builder.emit(Opcode::Return, &[1], span).unwrap();
    single_function_module("1()", Vec::new(), builder)
}

/// Builds a zero-register callee so ReturnUndefined exercises ordinary frame unwinding.
pub(in crate::tests) fn undefined_call_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::default();
    entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
    entry.emit(Opcode::Call, &[1, 0, 0], span).unwrap();
    entry.emit(Opcode::Return, &[1], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
    let mut callee = BytecodeBuilder::default();
    callee.emit(Opcode::ReturnUndefined, &[], span).unwrap();
    let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
    let entry_layout = FunctionLayout {
        register_count: entry_registers,
        ..FunctionLayout::default()
    };
    let callee_layout = FunctionLayout {
        register_count: callee_registers,
        ..FunctionLayout::default()
    };
    CompiledModule::new(
        Arc::from("function empty() {} empty();"),
        Vec::new(),
        Vec::new(),
        vec![
            CompiledFunctionTemplate::new(
                FunctionId::new(0),
                entry_bytecode,
                FunctionMetadata {
                    source_map: entry_source_map,
                    ..FunctionMetadata::new(FunctionKind::Script, entry_layout)
                },
            ),
            CompiledFunctionTemplate::new(
                FunctionId::new(1),
                callee_bytecode,
                FunctionMetadata {
                    source_map: callee_source_map,
                    ..FunctionMetadata::new(FunctionKind::Ordinary, callee_layout)
                },
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds one non-capturing function call with a contiguous single-argument window.
pub(in crate::tests) fn call_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 0 };
    let mut entry = BytecodeBuilder::default();
    entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
    entry.emit(Opcode::LoadImmediate, &[1, 40], span).unwrap();
    entry.emit(Opcode::Call, &[2, 0, 1], span).unwrap();
    entry.emit(Opcode::Return, &[2], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

    let mut callee = BytecodeBuilder::default();
    callee.emit(Opcode::LoadImmediate, &[1, 2], span).unwrap();
    callee.emit(Opcode::Add, &[2, 0, 1], span).unwrap();
    callee.emit(Opcode::Return, &[2], span).unwrap();
    let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();

    CompiledModule::new(
        Arc::from("function addTwo(value) { return value + 2; } addTwo(40);"),
        vec![],
        vec![],
        vec![
            CompiledFunctionTemplate::new(
                FunctionId::new(0),
                entry_bytecode,
                FunctionMetadata {
                    kind: FunctionKind::Script,
                    strictness: FunctionStrictness::Sloppy,
                    layout: FunctionLayout {
                        register_count: entry_registers,
                        ..FunctionLayout::default()
                    },
                    source_map: entry_source_map,
                    handlers: Arc::default(),
                    suspend_points: Arc::default(),
                    feedback_sites: Arc::default(),
                    binding_plan: Arc::default(),
                    environment_record_kind: EnvironmentRecordKind::Global,
                    environment_slots: Arc::default(),
                },
            ),
            CompiledFunctionTemplate::new(
                FunctionId::new(1),
                callee_bytecode,
                FunctionMetadata {
                    kind: FunctionKind::Ordinary,
                    strictness: FunctionStrictness::Sloppy,
                    layout: FunctionLayout {
                        register_count: callee_registers,
                        argument_count: 1,
                        ..FunctionLayout::default()
                    },
                    source_map: callee_source_map,
                    handlers: Arc::default(),
                    suspend_points: Arc::default(),
                    feedback_sites: Arc::default(),
                    binding_plan: Arc::default(),
                    environment_record_kind: EnvironmentRecordKind::Function,
                    environment_slots: Arc::default(),
                },
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds a callee throw so batch tests cover abrupt exit after an explicit frame switch.
pub(in crate::tests) fn throwing_call_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 0 };
    let mut entry = BytecodeBuilder::default();
    entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
    entry.emit(Opcode::Call, &[1, 0, 0], span).unwrap();
    entry.emit(Opcode::Return, &[1], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

    let mut callee = BytecodeBuilder::default();
    callee.emit(Opcode::LoadImmediate, &[0, 7], span).unwrap();
    callee.emit(Opcode::Throw, &[0], span).unwrap();
    let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();

    CompiledModule::new(
        Arc::from("function fail() { throw 7; } fail();"),
        vec![],
        vec![],
        vec![
            CompiledFunctionTemplate::new(
                FunctionId::new(0),
                entry_bytecode,
                FunctionMetadata {
                    kind: FunctionKind::Script,
                    strictness: FunctionStrictness::Sloppy,
                    layout: FunctionLayout {
                        register_count: entry_registers,
                        ..FunctionLayout::default()
                    },
                    source_map: entry_source_map,
                    handlers: Arc::default(),
                    suspend_points: Arc::default(),
                    feedback_sites: Arc::default(),
                    binding_plan: Arc::default(),
                    environment_record_kind: EnvironmentRecordKind::Global,
                    environment_slots: Arc::default(),
                },
            ),
            CompiledFunctionTemplate::new(
                FunctionId::new(1),
                callee_bytecode,
                FunctionMetadata {
                    kind: FunctionKind::Ordinary,
                    strictness: FunctionStrictness::Sloppy,
                    layout: FunctionLayout {
                        register_count: callee_registers,
                        ..FunctionLayout::default()
                    },
                    source_map: callee_source_map,
                    handlers: Arc::default(),
                    suspend_points: Arc::default(),
                    feedback_sites: Arc::default(),
                    binding_plan: Arc::default(),
                    environment_record_kind: EnvironmentRecordKind::Function,
                    environment_slots: Arc::default(),
                },
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds separate publisher/caller modules to exercise CodeId changes inside one dispatch batch.
pub(in crate::tests) fn cross_code_modules() -> (CompiledModule, CompiledModule) {
    let span = SourceSpan { start: 0, end: 0 };
    let mut publisher_entry = BytecodeBuilder::default();
    publisher_entry
        .emit(Opcode::CreateClosure, &[0, 1], span)
        .unwrap();
    publisher_entry
        .emit(Opcode::StoreScope, &[0, 0], span)
        .unwrap();
    publisher_entry.emit(Opcode::Return, &[0], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = publisher_entry.finish().unwrap();
    let mut published_function = BytecodeBuilder::default();
    published_function
        .emit(Opcode::LoadImmediate, &[0, 42], span)
        .unwrap();
    published_function.emit(Opcode::Return, &[0], span).unwrap();
    let (function_bytecode, function_source_map, function_registers) =
        published_function.finish().unwrap();
    let publisher = CompiledModule::new(
        Arc::from("publisher"),
        vec![],
        vec![Arc::from("answer")],
        vec![
            CompiledFunctionTemplate::new(
                FunctionId::new(0),
                entry_bytecode,
                FunctionMetadata {
                    kind: FunctionKind::Script,
                    strictness: FunctionStrictness::Sloppy,
                    layout: FunctionLayout {
                        register_count: entry_registers,
                        ..FunctionLayout::default()
                    },
                    source_map: entry_source_map,
                    handlers: Arc::default(),
                    suspend_points: Arc::default(),
                    feedback_sites: Arc::default(),
                    binding_plan: Arc::default(),
                    environment_record_kind: EnvironmentRecordKind::Global,
                    environment_slots: Arc::default(),
                },
            ),
            CompiledFunctionTemplate::new(
                FunctionId::new(1),
                function_bytecode,
                FunctionMetadata {
                    kind: FunctionKind::Ordinary,
                    strictness: FunctionStrictness::Sloppy,
                    layout: FunctionLayout {
                        register_count: function_registers,
                        ..FunctionLayout::default()
                    },
                    source_map: function_source_map,
                    handlers: Arc::default(),
                    suspend_points: Arc::default(),
                    feedback_sites: Arc::default(),
                    binding_plan: Arc::default(),
                    environment_record_kind: EnvironmentRecordKind::Function,
                    environment_slots: Arc::default(),
                },
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap();

    let mut caller_entry = BytecodeBuilder::default();
    caller_entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    caller_entry.emit(Opcode::Call, &[1, 0, 0], span).unwrap();
    caller_entry.emit(Opcode::Return, &[1], span).unwrap();
    let (caller_bytecode, caller_source_map, caller_registers) = caller_entry.finish().unwrap();
    let caller = CompiledModule::new(
        Arc::from("caller"),
        vec![],
        vec![Arc::from("answer")],
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            caller_bytecode,
            FunctionMetadata {
                kind: FunctionKind::Script,
                strictness: FunctionStrictness::Sloppy,
                layout: FunctionLayout {
                    register_count: caller_registers,
                    ..FunctionLayout::default()
                },
                source_map: caller_source_map,
                handlers: Arc::default(),
                suspend_points: Arc::default(),
                feedback_sites: Arc::default(),
                binding_plan: Arc::default(),
                environment_record_kind: EnvironmentRecordKind::Global,
                environment_slots: Arc::default(),
            },
        )],
        FunctionId::new(0),
    )
    .unwrap();
    (publisher, caller)
}

pub(in crate::tests) fn assert_call_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &call_module(),
            ExecutionBudget {
                fuel: 8,
                quantum: 8,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

pub(in crate::tests) fn assert_captured_environment_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &captured_environment_module(),
            ExecutionBudget {
                fuel: 32,
                quantum: 32,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
}

pub(in crate::tests) fn assert_throw_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &throwing_call_module(),
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Thrown(value) if value.as_i32() == Some(7)));
}

pub(in crate::tests) fn assert_cross_code_batch<const N: usize>() {
    let (publisher, caller) = cross_code_modules();
    let mut isolate = test_isolate();
    isolate
        .execute_with_batch::<N>(
            &publisher,
            ExecutionBudget {
                fuel: 8,
                quantum: 8,
            },
        )
        .unwrap();
    let outcome = isolate
        .execute_with_batch::<N>(
            &caller,
            ExecutionBudget {
                fuel: 8,
                quantum: 8,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

pub(in crate::tests) fn assert_property_batch<const N: usize>() {
    for module in [property_module(), function_property_module()] {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &module,
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }
}

pub(in crate::tests) fn assert_dynamic_property_batch<const N: usize>() {
    for module in [
        dynamic_property_module(),
        dynamic_string_property_module(),
        dynamic_numeric_property_module(),
    ] {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &module,
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }
}

pub(in crate::tests) fn assert_for_in_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &for_in_module(),
            ExecutionBudget {
                fuel: 32,
                quantum: 32,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(2)));
}

pub(in crate::tests) fn assert_method_receiver_batch<const N: usize>() {
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &method_receiver_module(),
            ExecutionBudget {
                fuel: 6,
                quantum: 6,
            },
        )
        .unwrap();
    assert_eq!(outcome, RunOutcome::BudgetExhausted);
    let receiver = isolate.fiber.frames[0].base;
    let receiver = isolate.fiber.registers[receiver as usize];
    assert_eq!(isolate.fiber.frames.last().unwrap().this_value, receiver);
}

pub(in crate::tests) fn assert_catch_batch<const N: usize>() {
    for module in [direct_catch_module(), cross_frame_catch_module()] {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &module,
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }
}

pub(in crate::tests) fn assert_construct_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &construct_module(),
            ExecutionBudget {
                fuel: 32,
                quantum: 32,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

pub(in crate::tests) fn assert_instanceof_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &instanceof_module(),
            ExecutionBudget {
                fuel: 6,
                quantum: 6,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::True)
    ));
}
