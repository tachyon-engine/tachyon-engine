use super::{
    accessors,
    fixtures::{arithmetic_module, test_isolate},
    *,
};

const SPAN: SourceSpan = SourceSpan { start: 0, end: 1 };

fn module(
    name: &'static str,
    builder: BytecodeBuilder,
    handlers: Vec<HandlerEntry>,
    max_handler_depth: u32,
    max_completion_depth: u32,
) -> CompiledModule {
    let (bytecode, source_map, register_count) = builder.finish().unwrap();
    let mut metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count,
            max_handler_depth,
            max_completion_depth,
            ..FunctionLayout::default()
        },
        source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    metadata.handlers = handlers.into();
    CompiledModule::new(
        Arc::from(name),
        Vec::new(),
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

fn normal_finally_module() -> CompiledModule {
    let mut builder = BytecodeBuilder::default();
    let protected_start = builder.emit(Opcode::Nop, &[], SPAN).unwrap();
    builder.emit(Opcode::LoadImmediate, &[0, 1], SPAN).unwrap();
    builder.emit(Opcode::EnterFinally, &[], SPAN).unwrap();
    let handler = builder.emit(Opcode::LoadImmediate, &[0, 2], SPAN).unwrap();
    builder.emit(Opcode::ResumeCompletion, &[], SPAN).unwrap();
    let handler_end = builder.current_offset().unwrap();
    builder.emit(Opcode::Return, &[0], SPAN).unwrap();
    module(
        "normal finally",
        builder,
        vec![HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            handler_end,
            kind: HandlerKind::Finally,
            environment_depth: 0,
        }],
        1,
        1,
    )
}

fn return_finally_module(override_return: bool) -> CompiledModule {
    let mut builder = BytecodeBuilder::default();
    let protected_start = builder.emit(Opcode::Nop, &[], SPAN).unwrap();
    builder.emit(Opcode::LoadImmediate, &[0, 7], SPAN).unwrap();
    builder.emit(Opcode::Return, &[0], SPAN).unwrap();
    let handler = builder.emit(Opcode::Nop, &[], SPAN).unwrap();
    if override_return {
        builder.emit(Opcode::LoadImmediate, &[1, 9], SPAN).unwrap();
        builder.emit(Opcode::Return, &[1], SPAN).unwrap();
    }
    builder.emit(Opcode::ResumeCompletion, &[], SPAN).unwrap();
    let handler_end = builder.current_offset().unwrap();
    builder.emit(Opcode::ReturnUndefined, &[], SPAN).unwrap();
    module(
        "return finally",
        builder,
        vec![HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            handler_end,
            kind: HandlerKind::Finally,
            environment_depth: 0,
        }],
        1,
        1,
    )
}

fn throw_finally_catch_module() -> CompiledModule {
    let mut builder = BytecodeBuilder::default();
    let outer_start = builder.emit(Opcode::Nop, &[], SPAN).unwrap();
    let inner_start = builder.emit(Opcode::LoadImmediate, &[0, 7], SPAN).unwrap();
    builder.emit(Opcode::Throw, &[0], SPAN).unwrap();
    let finalizer = builder.emit(Opcode::LoadImmediate, &[1, 1], SPAN).unwrap();
    builder.emit(Opcode::ResumeCompletion, &[], SPAN).unwrap();
    let finalizer_end = builder.current_offset().unwrap();
    let catch = builder.emit(Opcode::LoadException, &[2], SPAN).unwrap();
    builder.emit(Opcode::Add, &[3, 1, 2], SPAN).unwrap();
    builder.emit(Opcode::Return, &[3], SPAN).unwrap();
    module(
        "throw finally catch",
        builder,
        vec![
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
        ],
        2,
        1,
    )
}

fn throw_overrides_return_module() -> CompiledModule {
    let mut builder = BytecodeBuilder::default();
    let protected_start = builder.emit(Opcode::LoadImmediate, &[0, 7], SPAN).unwrap();
    builder.emit(Opcode::Return, &[0], SPAN).unwrap();
    let handler = builder.emit(Opcode::LoadImmediate, &[1, 9], SPAN).unwrap();
    builder.emit(Opcode::Throw, &[1], SPAN).unwrap();
    builder.emit(Opcode::ResumeCompletion, &[], SPAN).unwrap();
    let handler_end = builder.current_offset().unwrap();
    builder.emit(Opcode::ReturnUndefined, &[], SPAN).unwrap();
    module(
        "throw overrides return",
        builder,
        vec![HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            handler_end,
            kind: HandlerKind::Finally,
            environment_depth: 0,
        }],
        1,
        1,
    )
}

/// Builds break or continue crossing two protected finalizers in observable inner-to-outer order.
fn nested_control_module(opcode: Opcode) -> CompiledModule {
    let mut builder = BytecodeBuilder::default();
    let end = builder.new_label().unwrap();
    builder.emit(Opcode::LoadImmediate, &[0, 0], SPAN).unwrap();
    let outer_start = builder.emit(Opcode::Nop, &[], SPAN).unwrap();
    let inner_start = builder.emit(Opcode::Nop, &[], SPAN).unwrap();
    builder.emit_abrupt_jump(opcode, end, 0, SPAN).unwrap();
    let inner_handler = builder.emit(Opcode::LoadImmediate, &[0, 1], SPAN).unwrap();
    builder.emit(Opcode::ResumeCompletion, &[], SPAN).unwrap();
    let inner_end = builder.current_offset().unwrap();
    let outer_handler = builder.emit(Opcode::LoadImmediate, &[1, 10], SPAN).unwrap();
    builder.emit(Opcode::Mul, &[0, 0, 1], SPAN).unwrap();
    builder.emit(Opcode::LoadImmediate, &[2, 2], SPAN).unwrap();
    builder.emit(Opcode::Add, &[0, 0, 2], SPAN).unwrap();
    builder.emit(Opcode::ResumeCompletion, &[], SPAN).unwrap();
    let outer_end = builder.current_offset().unwrap();
    builder.bind_label(end).unwrap();
    builder.emit(Opcode::Return, &[0], SPAN).unwrap();
    module(
        "nested control finally",
        builder,
        vec![
            HandlerEntry {
                protected_start: outer_start,
                protected_end: outer_handler,
                handler: outer_handler,
                handler_end: outer_end,
                kind: HandlerKind::Finally,
                environment_depth: 0,
            },
            HandlerEntry {
                protected_start: inner_start,
                protected_end: inner_handler,
                handler: inner_handler,
                handler_end: inner_end,
                kind: HandlerKind::Finally,
                environment_depth: 0,
            },
        ],
        2,
        1,
    )
}

fn stale_completion_module() -> CompiledModule {
    let mut builder = BytecodeBuilder::default();
    let after_first = builder.new_label().unwrap();
    builder.emit(Opcode::LoadImmediate, &[0, 1], SPAN).unwrap();
    let first_start = builder.emit(Opcode::Nop, &[], SPAN).unwrap();
    builder.emit(Opcode::Return, &[0], SPAN).unwrap();
    let first_handler = builder.current_offset().unwrap();
    builder
        .emit_abrupt_jump(Opcode::BreakThroughFinally, after_first, 0, SPAN)
        .unwrap();
    builder.emit(Opcode::ResumeCompletion, &[], SPAN).unwrap();
    let first_end = builder.current_offset().unwrap();
    builder.bind_label(after_first).unwrap();
    let second_start = builder.emit(Opcode::Nop, &[], SPAN).unwrap();
    builder.emit(Opcode::EnterFinally, &[], SPAN).unwrap();
    let second_handler = builder.emit(Opcode::Nop, &[], SPAN).unwrap();
    builder.emit(Opcode::ResumeCompletion, &[], SPAN).unwrap();
    let second_end = builder.current_offset().unwrap();
    builder.emit(Opcode::LoadImmediate, &[1, 2], SPAN).unwrap();
    builder.emit(Opcode::Return, &[1], SPAN).unwrap();
    module(
        "stale completion",
        builder,
        vec![
            HandlerEntry {
                protected_start: first_start,
                protected_end: first_handler,
                handler: first_handler,
                handler_end: first_end,
                kind: HandlerKind::Finally,
                environment_depth: 0,
            },
            HandlerEntry {
                protected_start: second_start,
                protected_end: second_handler,
                handler: second_handler,
                handler_end: second_end,
                kind: HandlerKind::Finally,
                environment_depth: 0,
            },
        ],
        1,
        1,
    )
}

fn catch_inside_finalizer_module() -> CompiledModule {
    let mut builder = BytecodeBuilder::default();
    builder.emit(Opcode::LoadImmediate, &[0, 7], SPAN).unwrap();
    let outer_start = builder.emit(Opcode::Nop, &[], SPAN).unwrap();
    builder.emit(Opcode::Return, &[0], SPAN).unwrap();
    let outer_handler = builder.emit(Opcode::Nop, &[], SPAN).unwrap();
    let inner_start = builder.emit(Opcode::LoadImmediate, &[1, 1], SPAN).unwrap();
    builder.emit(Opcode::Throw, &[1], SPAN).unwrap();
    let catch = builder.emit(Opcode::LoadException, &[2], SPAN).unwrap();
    builder.emit(Opcode::ResumeCompletion, &[], SPAN).unwrap();
    let outer_end = builder.current_offset().unwrap();
    builder.emit(Opcode::ReturnUndefined, &[], SPAN).unwrap();
    module(
        "catch inside finally",
        builder,
        vec![
            HandlerEntry {
                protected_start: outer_start,
                protected_end: outer_handler,
                handler: outer_handler,
                handler_end: outer_end,
                kind: HandlerKind::Finally,
                environment_depth: 0,
            },
            HandlerEntry {
                protected_start: inner_start,
                protected_end: catch,
                handler: catch,
                handler_end: catch,
                kind: HandlerKind::Catch,
                environment_depth: 0,
            },
        ],
        2,
        1,
    )
}

fn saved_object_module() -> CompiledModule {
    let mut builder = BytecodeBuilder::default();
    let protected_start = builder.emit(Opcode::CreateObject, &[0], SPAN).unwrap();
    builder.emit(Opcode::Return, &[0], SPAN).unwrap();
    let handler = builder.emit(Opcode::CreateObject, &[1], SPAN).unwrap();
    builder.emit(Opcode::ResumeCompletion, &[], SPAN).unwrap();
    let handler_end = builder.current_offset().unwrap();
    builder.emit(Opcode::ReturnUndefined, &[], SPAN).unwrap();
    module(
        "saved object",
        builder,
        vec![HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            handler_end,
            kind: HandlerKind::Finally,
            environment_depth: 0,
        }],
        1,
        1,
    )
}

fn assert_i32<const N: usize>(module: &CompiledModule, expected: i32) {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            module,
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(expected)));
}

/// Exercises the synchronous completion matrix and A3 integration for one dispatch batch.
fn assert_finally_matrix<const N: usize>() {
    assert_i32::<N>(&normal_finally_module(), 2);
    assert_i32::<N>(&return_finally_module(false), 7);
    assert_i32::<N>(&return_finally_module(true), 9);
    assert_i32::<N>(&throw_finally_catch_module(), 8);
    assert_i32::<N>(&nested_control_module(Opcode::BreakThroughFinally), 12);
    assert_i32::<N>(&nested_control_module(Opcode::ContinueThroughFinally), 12);
    assert_i32::<N>(&stale_completion_module(), 2);
    assert_i32::<N>(&catch_inside_finalizer_module(), 7);
    let thrown = test_isolate()
        .execute_with_batch::<N>(
            &throw_overrides_return_module(),
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .unwrap();
    assert!(matches!(thrown, RunOutcome::Thrown(value) if value.as_i32() == Some(9)));

    let module = accessors::accessor_finally_throw_module();
    let mut isolate = test_isolate();
    accessors::install_bytecode_accessor(&mut isolate, &module, true, false);
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(5)));
}

#[test]
fn finally_completion_matrix_is_stable_for_every_dispatch_batch() {
    assert_finally_matrix::<1>();
    assert_finally_matrix::<2>();
    assert_finally_matrix::<4>();
    assert_finally_matrix::<8>();
    assert_finally_matrix::<16>();
}

#[test]
fn saved_return_payload_survives_forced_major_inside_finalizer() {
    let module = saved_object_module();
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .unwrap();
    let RunOutcome::Completed(value) = outcome else {
        panic!("saved object return must complete");
    };
    assert!(isolate.is_object_value(value));
}

#[test]
fn finally_entry_respects_host_completion_limit() {
    let config = IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(8 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096).with_max_completions(0),
        RealmLimits::new(64, 1_024),
    );
    let error = Isolate::new(config)
        .unwrap()
        .execute(
            &normal_finally_module(),
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .unwrap_err();
    assert_eq!(
        error,
        ExecutionError::CompletionStackLimit {
            limit: 0,
            requested: 1,
        }
    );
}

#[test]
fn frame_layout_caches_finally_without_growing_the_call_record() {
    assert_eq!(core::mem::size_of::<Frame>(), 104);
    let mut ordinary = test_isolate();
    ordinary
        .execute(
            &arithmetic_module(),
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .unwrap();
    assert!(!ordinary.fiber.frames[0].has_finally);

    let mut with_finally = test_isolate();
    with_finally
        .execute(
            &normal_finally_module(),
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .unwrap();
    assert!(with_finally.fiber.frames[0].has_finally);
}
