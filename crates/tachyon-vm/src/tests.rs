use std::sync::Arc;

use tachyon_bytecode::{
    BindingLocation, BindingPlanEntry, Bytecode, BytecodeBuilder, BytecodeConstant,
    CompiledFunctionTemplate, CompiledModule, FunctionId, FunctionKind, FunctionLayout,
    FunctionMetadata, OperandWidth, SourceSpan, encode_instruction,
};
use tachyon_gc::{ForcedCollectionMode, GcRef, HeapLimit, SPAN_SIZE_BYTES, Tracer};
use tachyon_value::RawHeapRef;

use super::*;

fn test_isolate() -> Isolate {
    Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(8 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("test isolate descriptors register")
}

fn arithmetic_module() -> CompiledModule {
    binary_module(Opcode::Add, "1 + 2")
}

fn less_than_module() -> CompiledModule {
    binary_module(Opcode::LessThan, "1 < 2")
}

/// Builds a closure whose empty activation inherits and mutates the entry environment.
fn captured_environment_module() -> CompiledModule {
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
fn typeof_module() -> CompiledModule {
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
fn string_constant_root_module() -> CompiledModule {
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
fn scoped_var_module() -> CompiledModule {
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
fn global_lexical_module() -> CompiledModule {
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
fn single_function_module(
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
fn binary_module(opcode: Opcode, source: &'static str) -> CompiledModule {
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
fn non_callable_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::default();
    builder.emit(Opcode::LoadImmediate, &[0, 1], span).unwrap();
    builder.emit(Opcode::Call, &[1, 0, 0], span).unwrap();
    builder.emit(Opcode::Return, &[1], span).unwrap();
    single_function_module("1()", Vec::new(), builder)
}

/// Builds a zero-register callee so ReturnUndefined exercises ordinary frame unwinding.
fn undefined_call_module() -> CompiledModule {
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
fn call_module() -> CompiledModule {
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
                },
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds a callee throw so batch tests cover abrupt exit after an explicit frame switch.
fn throwing_call_module() -> CompiledModule {
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
                },
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds separate publisher/caller modules to exercise CodeId changes inside one dispatch batch.
fn cross_code_modules() -> (CompiledModule, CompiledModule) {
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
            },
        )],
        FunctionId::new(0),
    )
    .unwrap();
    (publisher, caller)
}

fn assert_call_batch<const N: usize>() {
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

fn assert_captured_environment_batch<const N: usize>() {
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

fn assert_throw_batch<const N: usize>() {
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

fn assert_cross_code_batch<const N: usize>() {
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

fn assert_property_batch<const N: usize>() {
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

fn assert_dynamic_property_batch<const N: usize>() {
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

fn assert_for_in_batch<const N: usize>() {
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

fn assert_method_receiver_batch<const N: usize>() {
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

fn assert_catch_batch<const N: usize>() {
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

fn assert_construct_batch<const N: usize>() {
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

fn assert_instanceof_batch<const N: usize>() {
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

#[test]
fn interpreter_executes_int32_arithmetic() {
    assert_batch_result::<1>();
    assert_batch_result::<2>();
    assert_batch_result::<4>();
    assert_batch_result::<8>();
    assert_batch_result::<16>();
}

#[test]
fn int32_min_division_promotes_instead_of_overflowing_remainder() {
    let value =
        numeric_binary_hot(Opcode::Div, Value::from_i32(i32::MIN), Value::from_i32(-1)).unwrap();
    assert_eq!(value.as_f64(), Some(2_147_483_648.0));
}

#[test]
fn numeric_less_than_works_for_every_dispatch_batch() {
    assert_less_than_batch::<1>();
    assert_less_than_batch::<2>();
    assert_less_than_batch::<4>();
    assert_less_than_batch::<8>();
    assert_less_than_batch::<16>();
}

#[test]
fn typeof_and_string_equality_work_for_every_dispatch_batch() {
    assert_typeof_batch::<1>();
    assert_typeof_batch::<2>();
    assert_typeof_batch::<4>();
    assert_typeof_batch::<8>();
    assert_typeof_batch::<16>();
}

#[test]
fn script_var_declaration_works_for_every_dispatch_batch() {
    assert_scope_batch::<1>();
    assert_scope_batch::<2>();
    assert_scope_batch::<4>();
    assert_scope_batch::<8>();
    assert_scope_batch::<16>();
}

#[test]
fn global_lexical_access_works_for_every_dispatch_batch() {
    assert_global_lexical_batch::<1>();
    assert_global_lexical_batch::<2>();
    assert_global_lexical_batch::<4>();
    assert_global_lexical_batch::<8>();
    assert_global_lexical_batch::<16>();
}

#[test]
fn function_prototype_call_forwards_arguments_for_every_dispatch_batch() {
    assert_function_prototype_call_batch::<1>();
    assert_function_prototype_call_batch::<2>();
    assert_function_prototype_call_batch::<4>();
    assert_function_prototype_call_batch::<8>();
    assert_function_prototype_call_batch::<16>();
}

#[test]
fn native_continuation_resumes_for_every_dispatch_batch_and_forced_major() {
    assert_number_continuation_batch::<1>();
    assert_number_continuation_batch::<2>();
    assert_number_continuation_batch::<4>();
    assert_number_continuation_batch::<8>();
    assert_number_continuation_batch::<16>();
}

#[test]
fn native_continuation_throw_reaches_original_call_site_for_every_dispatch_batch() {
    assert_number_continuation_throw_batch::<1>();
    assert_number_continuation_throw_batch::<2>();
    assert_number_continuation_throw_batch::<4>();
    assert_number_continuation_throw_batch::<8>();
    assert_number_continuation_throw_batch::<16>();
}

#[test]
fn string_hint_continuation_resumes_for_every_dispatch_batch_and_forced_major() {
    assert_string_continuation_batch::<1>();
    assert_string_continuation_batch::<2>();
    assert_string_continuation_batch::<4>();
    assert_string_continuation_batch::<8>();
    assert_string_continuation_batch::<16>();
}

#[test]
fn numeric_unary_continuations_resume_for_every_dispatch_batch_and_forced_major() {
    assert_numeric_unary_continuation_batch::<1>();
    assert_numeric_unary_continuation_batch::<2>();
    assert_numeric_unary_continuation_batch::<4>();
    assert_numeric_unary_continuation_batch::<8>();
    assert_numeric_unary_continuation_batch::<16>();
}

#[test]
fn primitive_binary_continuations_resume_for_every_dispatch_batch_and_forced_major() {
    assert_primitive_binary_continuation_batch::<1>();
    assert_primitive_binary_continuation_batch::<2>();
    assert_primitive_binary_continuation_batch::<4>();
    assert_primitive_binary_continuation_batch::<8>();
    assert_primitive_binary_continuation_batch::<16>();
}

#[test]
fn bound_argument_prefix_forwards_for_every_dispatch_batch() {
    assert_bound_function_batch::<1>();
    assert_bound_function_batch::<2>();
    assert_bound_function_batch::<4>();
    assert_bound_function_batch::<8>();
    assert_bound_function_batch::<16>();
}

#[test]
fn array_push_method_call_is_stable_for_every_dispatch_batch() {
    assert_array_push_batch::<1>();
    assert_array_push_batch::<2>();
    assert_array_push_batch::<4>();
    assert_array_push_batch::<8>();
    assert_array_push_batch::<16>();
}

#[test]
fn strict_and_sloppy_this_binding_work_for_every_dispatch_batch() {
    assert_this_binding_batch::<1>();
    assert_this_binding_batch::<2>();
    assert_this_binding_batch::<4>();
    assert_this_binding_batch::<8>();
    assert_this_binding_batch::<16>();
}

#[test]
fn strict_and_sloppy_unresolved_assignment_work_for_every_dispatch_batch() {
    assert_reference_error_batch::<1>();
    assert_reference_error_batch::<2>();
    assert_reference_error_batch::<4>();
    assert_reference_error_batch::<8>();
    assert_reference_error_batch::<16>();
}

#[test]
fn non_callable_values_throw_type_error_for_every_dispatch_batch() {
    assert_non_callable_batch::<1>();
    assert_non_callable_batch::<2>();
    assert_non_callable_batch::<4>();
    assert_non_callable_batch::<8>();
    assert_non_callable_batch::<16>();
}

#[test]
fn native_error_constructor_hierarchy_survives_forced_major() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &native_error_constructor_module(),
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::True)
    ));
}

#[test]
fn native_function_intrinsics_survive_forced_major_before_forwarding() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &function_prototype_call_module(),
            ExecutionBudget {
                fuel: 7,
                quantum: 7,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

#[test]
fn boxed_number_data_and_properties_survive_forced_major_allocations() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let prototype = isolate.realm.number_prototype.unwrap();
    let boxed = isolate
        .allocate_number_object(Value::from_i32(-7), prototype, AllocationSpace::Young)
        .unwrap();
    isolate.fiber.registers.push(boxed);
    let key = isolate.intern_intrinsic_name(b"field").unwrap();
    isolate
        .set_own_data_property(boxed, key, Value::from_i32(11))
        .unwrap();
    assert_eq!(isolate.this_number_value(boxed).unwrap().as_i32(), Some(-7));
    assert_eq!(
        isolate
            .get_data_property(boxed, key)
            .unwrap()
            .unwrap()
            .as_i32(),
        Some(11)
    );
    assert_eq!(
        isolate.object_snapshot(boxed).unwrap().1.prototype,
        prototype
    );
}

#[test]
/// Keeps a pending Symbol description live before allocation and through a later full major.
fn symbol_description_survives_forced_major_allocations() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let description = isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"description").unwrap())
        .unwrap();
    let symbol = isolate.allocate_symbol(Some(description)).unwrap();
    isolate.fiber.registers.push(symbol);
    isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"collect").unwrap())
        .unwrap();
    let raw = symbol.as_heap_ref().unwrap();
    let symbol = isolate
        .heap
        .checked_reference(raw, isolate.types.symbol)
        .unwrap();
    let description = isolate.heap.with_running_scope(|scope| {
        let symbol = scope.root(symbol).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow(symbol, isolate.types.symbol)
                .map(|symbol| symbol.description.unwrap())
                .unwrap()
        })
    });
    let mut units = Vec::new();
    isolate
        .append_primitive_string_units(description, &mut units)
        .unwrap();
    assert_eq!(units, "description".encode_utf16().collect::<Vec<_>>());
}

#[test]
fn bound_function_payload_and_name_survive_forced_major_allocations() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &bound_function_call_module(),
            ExecutionBudget {
                fuel: 16,
                quantum: 16,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

#[test]
fn array_identity_and_property_storage_survive_forced_major_allocations() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &array_push_module(),
            ExecutionBudget {
                fuel: 8,
                quantum: 8,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(2)));
}

#[test]
fn array_flat_work_stack_survives_forced_major_allocations() {
    let mut isolate = test_isolate();
    let prototype = isolate.realm.array_prototype.unwrap();
    let inner = isolate
        .create_array_object_with_prototype(prototype)
        .unwrap();
    let outer = isolate
        .create_array_object_with_prototype(prototype)
        .unwrap();
    let zero = isolate.safe_integer_property_atom(0).unwrap();
    let length = isolate.length_atom().unwrap();
    isolate
        .set_own_data_property(inner, zero, Value::from_i32(42))
        .unwrap();
    isolate
        .set_own_data_property(inner, length, Value::from_i32(1))
        .unwrap();
    isolate.set_own_data_property(outer, zero, inner).unwrap();
    isolate
        .set_own_data_property(outer, length, Value::from_i32(1))
        .unwrap();
    isolate.fiber.registers = vec![outer, Value::from_i32(1)];
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let result = isolate
        .array_flat(&CallSite {
            caller_base: 0,
            destination: 0,
            callee: isolate.realm.array_flat.unwrap(),
            argument_base: 1,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 1,
            this_value: outer,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: WordOffset::new(0),
        })
        .unwrap();
    assert_eq!(
        isolate
            .get_data_property(result, zero)
            .unwrap()
            .and_then(Value::as_i32),
        Some(42)
    );
}

#[test]
fn nested_bound_construct_preserves_each_new_target_substitution() {
    let mut isolate = test_isolate();
    let target = isolate.realm.array_constructor.unwrap();
    let first = create_test_bound_function(&mut isolate, target);
    let second = create_test_bound_function(&mut isolate, first);

    let (resolved, new_target) = isolate
        .resolve_bound_construct_target(second, first)
        .unwrap();
    assert_eq!((resolved, new_target), (target, target));
    let (resolved, new_target) = isolate
        .resolve_bound_construct_target(second, second)
        .unwrap();
    assert_eq!((resolved, new_target), (target, target));
}

/// Exercises raw native helpers before the managed-error dispatch boundary.
#[test]
fn array_push_updates_existing_indexed_storage_and_length() {
    let mut isolate = test_isolate();
    let prototype = isolate.realm.array_prototype.unwrap();
    let array = isolate
        .create_array_object_with_prototype(prototype)
        .unwrap();
    let zero = isolate.property_key_atom(Value::from_i32(0)).unwrap();
    isolate
        .set_own_data_property(array, zero, Value::from_i32(1))
        .unwrap();
    let length = isolate.length_atom().unwrap();
    isolate
        .set_own_data_property(array, length, Value::from_i32(1))
        .unwrap();
    isolate.fiber.registers = vec![array, Value::from_i32(2), Value::from_i32(3)];
    let result = isolate
        .array_push(&CallSite {
            caller_base: 0,
            destination: 0,
            callee: isolate.realm.array_push.unwrap(),
            argument_base: 1,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 2,
            this_value: array,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: WordOffset::new(0),
        })
        .unwrap();
    assert_eq!(result.as_i32(), Some(3));
    assert_eq!(
        isolate
            .get_data_property(array, length)
            .unwrap()
            .unwrap()
            .as_i32(),
        Some(3)
    );
    let separator = isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"-").unwrap())
        .unwrap();
    isolate.fiber.registers.push(separator);
    let joined = isolate
        .array_join(&CallSite {
            caller_base: 0,
            destination: 0,
            callee: isolate.realm.array_join.unwrap(),
            argument_base: 3,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 1,
            this_value: array,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: WordOffset::new(0),
        })
        .unwrap();
    let expected = isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"1-2-3").unwrap())
        .unwrap();
    assert!(isolate.strict_equal_values(joined, expected).unwrap());
}

#[test]
/// Forces collection between loaded literals so pending and published caches must both trace.
fn loaded_string_constants_survive_forced_major_during_module_load() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &string_constant_root_module(),
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::False)
    ));
}

#[test]
fn isolate_owns_the_atom_table_created_from_mandatory_host_config() {
    let mut isolate = test_isolate();
    let mandatory_entries = isolate.atoms().stats().entries;
    let atom = isolate
        .atoms_mut()
        .try_intern(JsString::try_from_str("property").unwrap())
        .unwrap();

    assert_eq!(atom.index() as usize, mandatory_entries);
    assert_eq!(isolate.atoms().get(atom).unwrap().len(), 8);
    assert_eq!(isolate.atoms().stats().entries, mandatory_entries + 1);
}

#[test]
fn primitive_realm_intrinsics_use_stable_non_writable_slots() {
    let mut isolate = test_isolate();
    let infinity = isolate
        .atoms
        .try_intern(JsString::try_from_str("Infinity").unwrap())
        .unwrap();
    let slot = isolate.realm.resolve_intrinsic(infinity).unwrap();

    assert_eq!(
        isolate
            .scope_value(ScopeResolution {
                atom: infinity,
                lexical_slot: None,
                intrinsic_slot: Some(slot),
                global_slot: None,
            })
            .unwrap()
            .and_then(Value::as_f64),
        Some(f64::INFINITY)
    );
    assert_eq!(
        isolate.realm.set_intrinsic(slot, Value::from_i32(1)),
        Err(ExecutionError::ReadOnlyBinding(infinity))
    );
}

#[test]
fn var_declaration_does_not_publish_over_primitive_realm_intrinsics() {
    let mut isolate = test_isolate();
    let infinity = isolate
        .atoms
        .try_intern(JsString::try_from_str("Infinity").unwrap())
        .unwrap();
    isolate
        .declare_scope_resolution(ScopeResolution {
            atom: infinity,
            lexical_slot: None,
            intrinsic_slot: isolate.realm.resolve_intrinsic(infinity),
            global_slot: None,
        })
        .unwrap();
    assert!(isolate.realm.global_bindings.is_empty());
}

#[test]
/// Proves sparse atom publication resolves through stable slots without binding-order scans.
fn realm_global_slots_are_stable_and_atom_indexed() {
    let mut isolate = test_isolate();
    let unused = isolate
        .atoms
        .try_intern(JsString::try_from_str("unused-property-name").unwrap())
        .unwrap();
    let first = isolate
        .atoms
        .try_intern(JsString::try_from_str("first-global").unwrap())
        .unwrap();
    let second = isolate
        .atoms
        .try_intern(JsString::try_from_str("second-global").unwrap())
        .unwrap();

    isolate.realm.set(first, Value::from_i32(1)).unwrap();
    let first_slot = isolate.realm.resolve(first).unwrap();
    isolate.realm.set(second, Value::from_i32(2)).unwrap();
    isolate.realm.set(first, Value::from_i32(3)).unwrap();

    assert!(isolate.realm.resolve(unused).is_none());
    assert_eq!(isolate.realm.resolve(first), Some(first_slot));
    assert_eq!(
        isolate.realm.get_slot(first_slot).and_then(Value::as_i32),
        Some(3)
    );
    assert_eq!(
        isolate
            .realm
            .resolve(second)
            .and_then(|slot| isolate.realm.get_slot(slot))
            .and_then(Value::as_i32),
        Some(2)
    );
    assert_eq!(
        isolate.realm.global_bindings[first_slot.index()].name,
        first
    );
    assert_eq!(
        isolate.realm.global_slots_by_atom.len(),
        second.index() as usize + 1
    );
}

#[test]
/// Confirms loaded scope operands self-resolve once and retain the stable realm slot.
fn loaded_scope_resolution_caches_a_published_global_slot() {
    let mut isolate = test_isolate();
    let module = scoped_var_module();
    let code = isolate.load_module(&module).unwrap();
    assert!(
        isolate.loaded_code[code.index()].scope_resolutions[0]
            .global_slot
            .is_none()
    );

    let outcome = isolate
        .execute_loaded(
            code,
            ExecutionBudget {
                fuel: 16,
                quantum: 16,
            },
        )
        .unwrap();

    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(7)));
    let resolution = isolate.loaded_code[code.index()].scope_resolutions[0];
    let slot = resolution.global_slot.expect("executed binding is cached");
    assert_eq!(isolate.realm.resolve(resolution.atom), Some(slot));
    assert_eq!(
        isolate.realm.get_slot(slot).and_then(Value::as_i32),
        Some(7)
    );
}

#[cfg(feature = "opcode-profile")]
#[test]
fn opcode_profile_classifies_hot_and_terminal_slow_instructions() {
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute(
            &arithmetic_module(),
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));

    let profile = isolate.execution_profile();
    assert_eq!(
        profile.opcode(Opcode::LoadImmediate),
        OpcodeExecutionCounts {
            executed: 2,
            hot: 2,
            slow: 0,
            branch_taken: 0,
            branch_not_taken: 0,
        }
    );
    assert_eq!(profile.opcode(Opcode::Add).hot, 1);
    assert_eq!(profile.opcode(Opcode::Return).slow, 1);
    assert_eq!(
        profile
            .opcodes()
            .map(|(_, counts)| counts.executed)
            .sum::<u64>(),
        4
    );
    assert_eq!(profile.batch_cursor_binds(), 1);
    assert_eq!(profile.slow_flushes(), 1);
    assert_eq!(profile.terminal_slow_exits(), 1);
    assert_eq!(profile.batch_flushes(), 0);
    assert_eq!(profile.slow_rebinds(), 0);

    isolate.reset_execution_profile();
    assert_eq!(isolate.execution_profile(), &ExecutionProfile::default());
}

#[cfg(feature = "opcode-profile")]
#[test]
fn opcode_profile_counts_every_supported_dispatch_batch_exactly() {
    assert_arithmetic_profile_batch::<1>(4, 3);
    assert_arithmetic_profile_batch::<2>(2, 1);
    assert_arithmetic_profile_batch::<4>(1, 0);
    assert_arithmetic_profile_batch::<8>(1, 0);
    assert_arithmetic_profile_batch::<16>(1, 0);
}

#[cfg(feature = "opcode-profile")]
#[test]
fn opcode_profile_separates_same_and_changed_activation_rebinds() {
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<16>(
            &undefined_call_module(),
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::Undefined)
    ));
    let profile = isolate.execution_profile();
    assert_eq!(profile.opcode(Opcode::CreateClosure).slow, 1);
    assert_eq!(profile.opcode(Opcode::Call).slow, 1);
    assert_eq!(profile.opcode(Opcode::ReturnUndefined).slow, 1);
    assert_eq!(profile.opcode(Opcode::Return).slow, 1);
    assert_eq!(profile.same_activation_rebinds(), 1);
    assert_eq!(profile.activation_rebinds(), 2);
    assert_eq!(profile.slow_rebinds(), 3);
    assert_eq!(profile.terminal_slow_exits(), 1);
    assert_eq!(profile.fault_slow_exits(), 0);
    assert_profile_slow_exit_conservation(profile);
}

#[cfg(feature = "opcode-profile")]
#[test]
fn opcode_profile_records_budget_and_conditional_branch_outcomes() {
    let mut budgeted = test_isolate();
    assert_eq!(
        budgeted
            .execute_with_batch::<8>(
                &arithmetic_module(),
                ExecutionBudget {
                    fuel: 2,
                    quantum: 2,
                },
            )
            .unwrap(),
        RunOutcome::BudgetExhausted
    );
    assert_eq!(budgeted.execution_profile().budget_flushes(), 1);
    assert_eq!(
        budgeted
            .execution_profile()
            .opcodes()
            .map(|(_, counts)| counts.executed)
            .sum::<u64>(),
        2
    );

    for (condition, taken) in [(Opcode::LoadFalse, true), (Opcode::LoadTrue, false)] {
        let mut isolate = test_isolate();
        isolate
            .execute_with_batch::<8>(
                &logical_module(Opcode::JumpIfFalse, condition, None),
                ExecutionBudget {
                    fuel: u64::MAX,
                    quantum: u32::MAX,
                },
            )
            .unwrap();
        let counts = isolate.execution_profile().opcode(Opcode::JumpIfFalse);
        assert_eq!(counts.branch_taken, u64::from(taken));
        assert_eq!(counts.branch_not_taken, u64::from(!taken));
    }

    for (contents, taken) in [(Vec::new(), true), (vec![b'x' as u16], false)] {
        let mut isolate = test_isolate();
        isolate
            .execute_with_batch::<8>(
                &heap_string_branch_module(contents),
                ExecutionBudget {
                    fuel: u64::MAX,
                    quantum: u32::MAX,
                },
            )
            .unwrap();
        let counts = isolate.execution_profile().opcode(Opcode::JumpIfFalse);
        assert_eq!(counts.slow, 1);
        assert_eq!(counts.branch_taken, u64::from(taken));
        assert_eq!(counts.branch_not_taken, u64::from(!taken));
        assert_profile_slow_exit_conservation(isolate.execution_profile());
    }
}

#[test]
/// Pins the cursor's unsafe backing invariant across owner moves and Vec reallocations.
fn bytecode_cursor_survives_owner_and_container_moves() {
    let module = arithmetic_module();
    let bytecode = module.function(module.entry_function()).unwrap().bytecode();
    // SAFETY: `owners` retains the module's Arc-backed verified function through cursor use.
    let cursor = unsafe { BytecodeCursor::new(bytecode) };
    let mut owners = Vec::with_capacity(1);
    owners.push(module);
    owners.extend((0..32).map(|_| arithmetic_module()));

    // SAFETY: zero is the verified entry instruction start for this arithmetic function.
    let instruction = unsafe { cursor.decode(WordOffset::new(0)) };
    assert_eq!(instruction.opcode, Opcode::LoadImmediate);
    assert_eq!(owners.len(), 33);
}

#[test]
/// Pins the unchecked register cursor to one prevalidated, non-reallocating activation window.
fn register_window_checks_edges_before_unchecked_access() {
    let mut registers = [
        Value::from_i32(10),
        Value::from_i32(20),
        Value::from_i32(30),
        Value::from_i32(40),
    ];
    {
        let mut window = RegisterWindow::new(&mut registers, 1, 3).unwrap();
        // SAFETY: indices zero and two are the exact first/last slots in the checked window,
        // and the fixed array cannot move or change length while `window` is used.
        unsafe {
            assert_eq!(window.read(0).as_i32(), Some(20));
            assert_eq!(window.read(2).as_i32(), Some(40));
            window.write(2, Value::from_i32(99));
        }
    }
    assert_eq!(registers[3].as_i32(), Some(99));
    assert!(RegisterWindow::new(&mut registers, 2, 3).is_none());
    assert!(RegisterWindow::new(&mut registers, 4, 0).is_some());
    assert!(RegisterWindow::new(&mut registers, 5, 0).is_none());
}

#[test]
/// Differentials every allocation-free opcode against the checked dispatcher implementation.
fn verified_hot_kernel_matches_checked_dispatch() {
    let i = Value::from_i32;
    let immediate = Value::from_immediate;
    let cases = [
        (Opcode::Nop, [0, 0, 0], [i(1), i(2), i(3)]),
        (Opcode::LoadUndefined, [0, 0, 0], [i(1), i(2), i(3)]),
        (Opcode::LoadNull, [0, 0, 0], [i(1), i(2), i(3)]),
        (Opcode::LoadFalse, [0, 0, 0], [i(1), i(2), i(3)]),
        (Opcode::LoadTrue, [0, 0, 0], [i(1), i(2), i(3)]),
        (
            Opcode::LoadImmediate,
            [0, i32::MIN as u32, 0],
            [i(1), i(2), i(3)],
        ),
        (Opcode::Move, [0, 0, 0], [i(7), i(2), i(3)]),
        (
            Opcode::Not,
            [0, 1, 0],
            [i(7), immediate(Immediate::False), i(3)],
        ),
        (Opcode::Negate, [0, 1, 0], [i(7), i(i32::MIN), i(3)]),
        (Opcode::BitwiseNot, [0, 1, 0], [i(7), i(12), i(3)]),
        (Opcode::ToNumber, [0, 1, 0], [i(7), i(-0), i(3)]),
        (Opcode::Add, [0, 0, 1], [i(i32::MAX), i(1), i(3)]),
        (Opcode::Sub, [0, 0, 1], [i(i32::MIN), i(1), i(3)]),
        (Opcode::Mul, [0, 0, 1], [i(100_000), i(100_000), i(3)]),
        (Opcode::Div, [0, 0, 1], [i(i32::MIN), i(-1), i(3)]),
        (Opcode::BitwiseAnd, [0, 0, 1], [i(0b1100), i(0b1010), i(3)]),
        (Opcode::BitwiseOr, [0, 0, 1], [i(0b1100), i(0b1010), i(3)]),
        (Opcode::BitwiseXor, [0, 0, 1], [i(0b1100), i(0b1010), i(3)]),
        (Opcode::ShiftLeft, [0, 0, 1], [i(-1), i(3), i(3)]),
        (Opcode::ShiftRight, [0, 0, 1], [i(-8), i(2), i(3)]),
        (Opcode::ShiftRightUnsigned, [0, 0, 1], [i(-1), i(1), i(3)]),
        (Opcode::Remainder, [0, 0, 1], [i(-7), i(4), i(3)]),
        (Opcode::Exponentiate, [0, 0, 1], [i(3), i(4), i(3)]),
        (Opcode::LessThan, [0, 0, 1], [i(-1), i(1), i(3)]),
        (Opcode::GreaterThan, [0, 0, 1], [i(2), i(1), i(3)]),
        (Opcode::LessEqual, [0, 0, 1], [i(1), i(1), i(3)]),
        (Opcode::GreaterEqual, [0, 0, 1], [i(1), i(1), i(3)]),
        (
            Opcode::StrictEqual,
            [0, 1, 2],
            [i(7), immediate(Immediate::Null), immediate(Immediate::Null)],
        ),
        (Opcode::Jump, [9, 0, 0], [i(1), i(2), i(3)]),
        (
            Opcode::JumpIfFalse,
            [0, 9, 0],
            [immediate(Immediate::False), i(2), i(3)],
        ),
        (
            Opcode::JumpIfTrue,
            [0, 9, 0],
            [immediate(Immediate::True), i(2), i(3)],
        ),
        (Opcode::JumpIfNotNullish, [0, 9, 0], [i(1), i(2), i(3)]),
    ];
    let module = arithmetic_module();

    for (opcode, operands, initial) in cases {
        let mut checked = test_isolate();
        let code = checked.load_module(&module).unwrap();
        checked.enter(code, module.entry_function()).unwrap();
        checked.fiber.registers.copy_from_slice(&initial);
        let fallthrough = WordOffset::new(1);
        checked.flush_cursor_pc(fallthrough);
        assert_eq!(
            checked.dispatch(code, WordOffset::new(0), opcode, operands, 0),
            Ok(None),
            "{opcode:?}"
        );
        let checked_pc = checked.fiber.frames.last().unwrap().pc;

        let mut fast_registers = initial;
        let mut window = RegisterWindow::new(&mut fast_registers, 0, 3).unwrap();
        let instruction = DecodedInstruction {
            opcode,
            width: OperandWidth::Compact,
            operands,
            operand_count: opcode.operand_count() as u8,
            word_len: 1,
        };
        let mut fast_pc = fallthrough;
        // SAFETY: the local three-slot array covers every operand in this explicit case table
        // and cannot move or resize before the hot operation returns.
        let control =
            unsafe { execute_verified_hot_instruction(&mut window, instruction, &mut fast_pc) };
        assert_eq!(control, HotControl::Continue, "{opcode:?}");
        assert_eq!(
            fast_registers,
            checked.fiber.registers.as_slice(),
            "{opcode:?}"
        );
        assert_eq!(fast_pc, checked_pc, "{opcode:?}");
    }

    let heap_value = Value::from_heap_ref(RawHeapRef::new(1).unwrap());
    let mut registers = [heap_value, heap_value, Value::from_i32(0)];
    let mut window = RegisterWindow::new(&mut registers, 0, 3).unwrap();
    for opcode in [Opcode::Not, Opcode::JumpIfFalse, Opcode::JumpIfTrue] {
        let instruction = DecodedInstruction {
            opcode,
            width: OperandWidth::Compact,
            operands: [0, 1, 0],
            operand_count: opcode.operand_count() as u8,
            word_len: 1,
        };
        // SAFETY: the fixed window covers the operands; heap-tagged truthiness must return Slow
        // before dereferencing the synthetic logical address.
        assert_eq!(
            unsafe {
                execute_verified_hot_instruction(&mut window, instruction, &mut WordOffset::new(1))
            },
            HotControl::Slow
        );
    }
}

#[test]
fn interpreter_stops_at_exact_budget_boundary() {
    assert_batch_budget::<1>();
    assert_batch_budget::<2>();
    assert_batch_budget::<4>();
    assert_batch_budget::<8>();
    assert_batch_budget::<16>();
}

#[test]
fn interpreter_rejects_zero_batch_without_panicking() {
    let result = test_isolate().execute_with_batch::<0>(
        &arithmetic_module(),
        ExecutionBudget {
            fuel: 1,
            quantum: 1,
        },
    );
    assert_eq!(
        result,
        Err(ExecutionError::InvalidDispatchBatch { batch: 0 })
    );
}

#[test]
fn public_execute_uses_the_tuned_non_scalar_dispatch_batch() {
    assert_eq!(tuning::dispatch::DEFAULT_DISPATCH_BATCH, 8);
    let outcome = test_isolate()
        .execute(
            &arithmetic_module(),
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
}

#[test]
fn max_budget_sentinel_uses_the_unbounded_loop_without_changing_results() {
    let outcome = test_isolate()
        .execute(
            &arithmetic_module(),
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
}

#[test]
fn interpreter_restarts_dispatch_after_conditional_jumps() {
    assert_conditional_batch::<1>();
    assert_conditional_batch::<2>();
    assert_conditional_batch::<4>();
    assert_conditional_batch::<8>();
    assert_conditional_batch::<16>();
}

#[test]
fn interpreter_restarts_dispatch_after_backward_jumps() {
    assert_backedge_batch::<1>();
    assert_backedge_batch::<2>();
    assert_backedge_batch::<4>();
    assert_backedge_batch::<8>();
    assert_backedge_batch::<16>();
}

#[test]
fn for_in_iterator_loop_is_stable_for_every_dispatch_batch() {
    assert_for_in_batch::<1>();
    assert_for_in_batch::<2>();
    assert_for_in_batch::<4>();
    assert_for_in_batch::<8>();
    assert_for_in_batch::<16>();
}

#[test]
fn for_in_iterator_and_returned_keys_survive_forced_major() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &for_in_module(),
            ExecutionBudget {
                fuel: 32,
                quantum: 32,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(2)));
}

#[test]
fn logical_short_circuit_preserves_operands_for_every_dispatch_batch() {
    assert_logical_batch::<1>();
    assert_logical_batch::<2>();
    assert_logical_batch::<4>();
    assert_logical_batch::<8>();
    assert_logical_batch::<16>();
}

#[test]
fn switch_dispatch_chain_is_stable_for_every_dispatch_batch() {
    assert_switch_batch::<1>();
    assert_switch_batch::<2>();
    assert_switch_batch::<4>();
    assert_switch_batch::<8>();
    assert_switch_batch::<16>();
}

#[test]
fn explicit_function_frames_work_for_every_dispatch_batch() {
    assert_call_batch::<1>();
    assert_call_batch::<2>();
    assert_call_batch::<4>();
    assert_call_batch::<8>();
    assert_call_batch::<16>();
}

#[test]
fn zero_register_undefined_returns_work_for_every_dispatch_batch() {
    assert_undefined_call_batch::<1>();
    assert_undefined_call_batch::<2>();
    assert_undefined_call_batch::<4>();
    assert_undefined_call_batch::<8>();
    assert_undefined_call_batch::<16>();
}

#[test]
fn captured_environments_work_for_every_dispatch_batch() {
    assert_captured_environment_batch::<1>();
    assert_captured_environment_batch::<2>();
    assert_captured_environment_batch::<4>();
    assert_captured_environment_batch::<8>();
    assert_captured_environment_batch::<16>();
}

#[test]
fn captured_environment_survives_forced_major_allocation() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &captured_environment_module(),
            ExecutionBudget {
                fuel: 32,
                quantum: 32,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
}

#[test]
fn callee_throw_exits_every_dispatch_batch_without_native_unwind() {
    assert_throw_batch::<1>();
    assert_throw_batch::<2>();
    assert_throw_batch::<4>();
    assert_throw_batch::<8>();
    assert_throw_batch::<16>();
}

#[test]
fn cross_module_call_switches_code_for_every_dispatch_batch() {
    assert_cross_code_batch::<1>();
    assert_cross_code_batch::<2>();
    assert_cross_code_batch::<4>();
    assert_cross_code_batch::<8>();
    assert_cross_code_batch::<16>();
}

#[test]
fn ordinary_property_replacement_and_update_work_for_every_dispatch_batch() {
    assert_property_batch::<1>();
    assert_property_batch::<2>();
    assert_property_batch::<4>();
    assert_property_batch::<8>();
    assert_property_batch::<16>();
}

#[test]
fn computed_property_access_works_for_every_dispatch_batch() {
    assert_dynamic_property_batch::<1>();
    assert_dynamic_property_batch::<2>();
    assert_dynamic_property_batch::<4>();
    assert_dynamic_property_batch::<8>();
    assert_dynamic_property_batch::<16>();
}

#[test]
/// Forces collection during first numeric-key publication to exercise the shared rooting path.
fn computed_property_publication_roots_receiver_across_forced_major() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &dynamic_property_module(),
            ExecutionBudget {
                fuel: 8,
                quantum: 8,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

#[test]
fn integer_property_keys_preserve_ecmascript_decimal_spelling() {
    for (value, expected) in [
        (i32::MIN, "-2147483648"),
        (-1, "-1"),
        (0, "0"),
        (i32::MAX, "2147483647"),
    ] {
        let key = Int32PropertyKey::new(value);
        assert_eq!(key.as_bytes(), expected.as_bytes());
    }
}

#[test]
fn method_calls_preserve_receiver_for_every_dispatch_batch() {
    assert_method_receiver_batch::<1>();
    assert_method_receiver_batch::<2>();
    assert_method_receiver_batch::<4>();
    assert_method_receiver_batch::<8>();
    assert_method_receiver_batch::<16>();
}

#[test]
fn catch_dispatch_and_cross_frame_throw_work_for_every_dispatch_batch() {
    assert_catch_batch::<1>();
    assert_catch_batch::<2>();
    assert_catch_batch::<4>();
    assert_catch_batch::<8>();
    assert_catch_batch::<16>();
}

#[test]
fn construct_receiver_and_primitive_return_work_for_every_dispatch_batch() {
    assert_construct_batch::<1>();
    assert_construct_batch::<2>();
    assert_construct_batch::<4>();
    assert_construct_batch::<8>();
    assert_construct_batch::<16>();
}

#[test]
fn instanceof_walks_prototypes_for_every_dispatch_batch() {
    assert_instanceof_batch::<1>();
    assert_instanceof_batch::<2>();
    assert_instanceof_batch::<4>();
    assert_instanceof_batch::<8>();
    assert_instanceof_batch::<16>();
}

#[test]
/// Forced major collections cover closure prototype creation and receiver chain publication.
fn instanceof_prototype_chain_survives_forced_major() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
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

#[test]
/// Plain closure creation stays allocation-light until prototype observation materializes it.
fn function_prototype_is_lazily_materialized_with_constructor_back_reference() {
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<8>(
            &call_module(),
            ExecutionBudget {
                fuel: 1,
                quantum: 1,
            },
        )
        .unwrap();
    assert_eq!(outcome, RunOutcome::BudgetExhausted);
    let function = isolate.fiber.registers[0];
    let reference = isolate
        .heap
        .checked_reference(function.as_heap_ref().unwrap(), isolate.types.function)
        .unwrap();
    let before = isolate.heap.with_running_scope(|scope| {
        let function = scope.root(reference).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow(function, isolate.types.function)
                .unwrap()
                .function_prototype
        })
    });
    assert!(before.is_none());

    let prototype_atom = isolate.prototype_atom().unwrap();
    let prototype = isolate
        .get_data_property(function, prototype_atom)
        .unwrap()
        .unwrap();
    let constructor_atom = isolate.constructor_atom().unwrap();
    assert_eq!(
        isolate
            .get_data_property(prototype, constructor_atom)
            .unwrap(),
        Some(function)
    );
}

#[test]
fn construct_receiver_stays_rooted_across_forced_major_property_allocation() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &construct_module(),
            ExecutionBudget {
                fuel: 32,
                quantum: 32,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

#[test]
fn property_publication_roots_receiver_and_heap_value_across_forced_major() {
    for module in [
        heap_value_property_module(),
        function_heap_value_property_module(),
    ] {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        let RunOutcome::Completed(child) = outcome else {
            panic!("property fixture must complete");
        };
        assert!(isolate.object_snapshot(child).is_ok());
    }
}

#[test]
/// Confirms verifier-produced stack depths become allocation reservations before dispatch.
fn entry_reserves_verified_execution_windows() {
    let layout = FunctionLayout {
        register_count: 2,
        max_handler_depth: 3,
        max_completion_depth: 4,
        ..FunctionLayout::default()
    };
    let mut isolate = test_isolate();
    let module = state_module(FunctionKind::Module, layout);
    let code = isolate.load_module(&module).unwrap();
    isolate.enter(code, FunctionId::new(0)).unwrap();

    assert!(isolate.fiber.frames.capacity() >= 1);
    assert!(isolate.fiber.registers.capacity() >= 2);
    assert!(isolate.fiber.handlers.capacity() >= 3);
    assert!(isolate.fiber.completions.capacity() >= 4);
    assert_eq!(
        isolate.fiber.frames[0].strictness,
        FunctionStrictness::Strict
    );
}

#[test]
/// Exercises every fiber-owned GC edge with a tracer that simulates object relocation.
fn fiber_trace_roots_visits_execution_state_without_native_stack_scanning() {
    let layout = FunctionLayout {
        register_count: 1,
        max_handler_depth: 1,
        max_completion_depth: 2,
        ..FunctionLayout::default()
    };
    let mut isolate = test_isolate();
    let module = state_module(FunctionKind::Script, layout);
    let code = isolate.load_module(&module).unwrap();
    isolate.enter(code, FunctionId::new(0)).unwrap();
    let raw = RawHeapRef::new(16).expect("valid logical address");
    isolate.fiber.registers[0] = Value::from_heap_ref(raw);
    let global = isolate
        .atoms
        .try_intern(JsString::try_from_str("global").unwrap())
        .unwrap();
    isolate
        .realm
        .set(global, Value::from_heap_ref(raw))
        .unwrap();
    let frame = isolate.fiber.frames.last_mut().expect("entry frame exists");
    // SAFETY: this test never dereferences the synthetic environment reference; it only checks
    // that the exact tracing contract rewrites every encoded fiber edge.
    frame.environment = Some(unsafe { GcRef::from_raw_unchecked(raw) });
    frame.return_register = Some(RegisterId::new(0));
    frame.this_value = Value::from_heap_ref(raw);
    frame.new_target = Value::from_heap_ref(raw);
    isolate.fiber.handlers.push(ActiveHandler {
        handler_index: 0,
        frame_depth: 1,
        environment_depth: 1,
    });
    isolate.fiber.completions.extend([
        Completion::Return(Value::from_heap_ref(raw)),
        Completion::Throw(Value::from_heap_ref(raw)),
    ]);
    isolate.fiber.pending_exception = Some(Value::from_heap_ref(raw));
    let mut tracer = RewritingTracer;

    isolate.trace_roots(&mut tracer);

    let rewritten = RawHeapRef::new(32).expect("valid logical address");
    assert_eq!(isolate.fiber.registers[0].as_heap_ref(), Some(rewritten));
    let frame = isolate.fiber.frames.last().expect("entry frame exists");
    assert_eq!(frame.environment.map(GcRef::raw), Some(rewritten));
    assert_eq!(frame.this_value.as_heap_ref(), Some(rewritten));
    assert_eq!(frame.new_target.as_heap_ref(), Some(rewritten));
    assert!(matches!(
        isolate.fiber.completions[0],
        Completion::Return(value) if value.as_heap_ref() == Some(rewritten)
    ));
    assert_eq!(
        isolate.fiber.pending_exception.and_then(Value::as_heap_ref),
        Some(rewritten)
    );
    assert!(matches!(
        isolate.fiber.completions[1],
        Completion::Throw(value) if value.as_heap_ref() == Some(rewritten)
    ));
    assert_eq!(
        isolate
            .realm
            .resolve(global)
            .and_then(|slot| isolate.realm.get_slot(slot))
            .and_then(Value::as_heap_ref),
        Some(rewritten)
    );
}

struct RewritingTracer;

impl Tracer for RewritingTracer {
    fn trace_value(&mut self, value: &mut Value) {
        if let Some(reference) = value.as_heap_ref() {
            *value = Value::from_heap_ref(rewrite(reference));
        }
    }

    fn trace_raw_heap_ref(&mut self, reference: &mut RawHeapRef) {
        *reference = rewrite(*reference);
    }

    fn trace_weak_raw_heap_ref(&mut self, reference: &mut Option<RawHeapRef>) {
        *reference = reference.map(rewrite);
    }

    fn trace_ephemeron(&mut self, key: &mut Option<RawHeapRef>, value: &mut Value) {
        *key = key.map(rewrite);
        self.trace_value(value);
    }

    fn trace_finalization(&mut self, target: &mut Option<RawHeapRef>, held_value: &mut Value) {
        *target = target.map(rewrite);
        self.trace_value(held_value);
    }
}

fn rewrite(reference: RawHeapRef) -> RawHeapRef {
    RawHeapRef::new(reference.offset() + 16).expect("test offset stays non-zero")
}

/// Checks exact cursor boundaries for one arithmetic program under a selected batch size.
#[cfg(feature = "opcode-profile")]
fn assert_arithmetic_profile_batch<const N: usize>(expected_binds: u64, expected_flushes: u64) {
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &arithmetic_module(),
            ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    let profile = isolate.execution_profile();
    assert_eq!(profile.batch_cursor_binds(), expected_binds);
    assert_eq!(profile.batch_flushes(), expected_flushes);
    assert_eq!(profile.slow_flushes(), 1);
    assert_eq!(profile.terminal_slow_exits(), 1);
    assert_eq!(profile.fault_slow_exits(), 0);
    assert_profile_slow_exit_conservation(profile);
}

#[cfg(feature = "opcode-profile")]
fn assert_profile_slow_exit_conservation(profile: &ExecutionProfile) {
    let slow = profile
        .opcodes()
        .map(|(_, counts)| counts.slow)
        .sum::<u64>();
    assert_eq!(
        slow,
        profile.slow_rebinds() + profile.terminal_slow_exits() + profile.fault_slow_exits()
    );
}

fn assert_batch_result<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &arithmetic_module(),
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
}

fn assert_less_than_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &less_than_module(),
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::True)
    ));
}

fn assert_typeof_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &typeof_module(),
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

fn assert_scope_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &scoped_var_module(),
            ExecutionBudget {
                fuel: 6,
                quantum: 6,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(7)));
}

fn assert_global_lexical_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &global_lexical_module(),
            ExecutionBudget {
                fuel: 5,
                quantum: 5,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

fn assert_function_prototype_call_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &function_prototype_call_module(),
            ExecutionBudget {
                fuel: 7,
                quantum: 7,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

/// Exercises callback-local catch, continuation resume, and GC tracing for one dispatch batch.
fn assert_number_continuation_batch<const N: usize>() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<N>(
            &number_continuation_module(),
            ExecutionBudget {
                fuel: 24,
                quantum: 24,
            },
        )
        .unwrap();
    assert!(
        matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ),
        "unexpected continuation outcome: {outcome:?}"
    );
}

/// Exercises a callback throw reaching the handler around the original native call site.
fn assert_number_continuation_throw_batch<const N: usize>() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<N>(
            &number_continuation_throw_module(),
            ExecutionBudget {
                fuel: 16,
                quantum: 16,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

/// Exercises String's string-hint callback ordering and continuation tracing for one batch.
fn assert_string_continuation_batch<const N: usize>() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<N>(
            &string_continuation_module(),
            ExecutionBudget {
                fuel: 20,
                quantum: 20,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::True)
    ));
}

/// Exercises numeric unary callback resume and tracing for one dispatch batch.
fn assert_numeric_unary_continuation_batch<const N: usize>() {
    for (opcode, expected) in [
        (Opcode::ToNumber, 7),
        (Opcode::Negate, -7),
        (Opcode::BitwiseNot, -8),
    ] {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute_with_batch::<N>(
                &numeric_unary_continuation_module(opcode, expected),
                ExecutionBudget {
                    fuel: 20,
                    quantum: 20,
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

/// Exercises left and right callback resume while the pending operand remains traced.
fn assert_primitive_binary_continuation_batch<const N: usize>() {
    for module in [
        numeric_binary_continuation_module(),
        add_continuation_module(),
        relational_continuation_module(),
    ] {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute_with_batch::<N>(
                &module,
                ExecutionBudget {
                    fuel: 40,
                    quantum: 40,
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

fn assert_bound_function_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &bound_function_call_module(),
            ExecutionBudget {
                fuel: 16,
                quantum: 16,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

fn assert_array_push_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &array_push_module(),
            ExecutionBudget {
                fuel: 8,
                quantum: 8,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(2)));
}

/// Creates a zero-argument bound exotic through the production allocation path.
fn create_test_bound_function(isolate: &mut Isolate, target: Value) -> Value {
    let undefined = Value::from_immediate(Immediate::Undefined);
    isolate.fiber.registers.resize(3, undefined);
    isolate.fiber.registers[1] = target;
    let bound = isolate
        .create_bound_function(&CallSite {
            caller_base: 0,
            destination: 0,
            callee: isolate.realm.function_prototype_bind.unwrap(),
            argument_base: 2,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 0,
            this_value: target,
            new_target: undefined,
            construct_receiver: None,
            call_site: WordOffset::new(0),
        })
        .unwrap();
    isolate.fiber.registers[1] = bound;
    bound
}

/// Checks both nullish substitution and strict preservation in one dispatch monomorphization.
fn assert_this_binding_batch<const N: usize>() {
    let mut sloppy = test_isolate();
    let global_object = sloppy.realm.global_object.unwrap();
    let outcome = sloppy
        .execute_with_batch::<N>(
            &this_binding_module(FunctionStrictness::Sloppy),
            ExecutionBudget {
                fuel: 7,
                quantum: 7,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value == global_object));

    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &this_binding_module(FunctionStrictness::Strict),
            ExecutionBudget {
                fuel: 7,
                quantum: 7,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::Undefined)
    ));
}

/// Confirms dispatch-level error conversion preserves constructor identity for each batch.
fn assert_reference_error_batch<const N: usize>() {
    let mut isolate = test_isolate();
    let expected = isolate
        .realm
        .error_intrinsics
        .get(NativeErrorKind::Reference)
        .constructor
        .unwrap();
    let outcome = isolate
        .execute_with_batch::<N>(
            &unresolved_assignment_module(FunctionStrictness::Strict),
            ExecutionBudget {
                fuel: 2,
                quantum: 2,
            },
        )
        .unwrap();
    let RunOutcome::Thrown(error) = outcome else {
        panic!("strict unresolved assignment must throw");
    };
    assert_eq!(
        isolate.native_error_kind(error).unwrap(),
        Some(NativeErrorKind::Reference)
    );
    let constructor_atom = isolate.constructor_atom().unwrap();
    let constructor = isolate.get_data_property(error, constructor_atom).unwrap();
    assert_eq!(constructor, Some(expected));

    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &unresolved_assignment_module(FunctionStrictness::Sloppy),
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

/// Confirms the raw callable fast path retains language-level error conversion.
fn assert_non_callable_batch<const N: usize>() {
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &non_callable_module(),
            ExecutionBudget {
                fuel: 2,
                quantum: 2,
            },
        )
        .unwrap();
    let RunOutcome::Thrown(error) = outcome else {
        panic!("integer call must throw");
    };
    assert_eq!(
        isolate.native_error_kind(error).unwrap(),
        Some(NativeErrorKind::Type)
    );
}

/// Confirms a zero-register callee returns undefined through the caller destination.
fn assert_undefined_call_batch<const N: usize>() {
    let module = undefined_call_module();
    assert_eq!(
        module
            .function(FunctionId::new(1))
            .unwrap()
            .layout()
            .register_count,
        0
    );
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::Undefined)
    ));
}

fn assert_batch_budget<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &arithmetic_module(),
            ExecutionBudget {
                fuel: 3,
                quantum: 3,
            },
        )
        .unwrap();
    assert_eq!(outcome, RunOutcome::BudgetExhausted);
}

fn assert_conditional_batch<const N: usize>() {
    for (test, expected) in [(Opcode::LoadTrue, 1), (Opcode::LoadFalse, 2)] {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &conditional_module(test),
                ExecutionBudget {
                    fuel: 6,
                    quantum: 6,
                },
            )
            .unwrap();
        assert!(
            matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(expected))
        );
    }
}

fn assert_backedge_batch<const N: usize>() {
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &backedge_module(),
            ExecutionBudget {
                fuel: 32,
                quantum: 32,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
}

/// Checks both paths of all logical branches under one dispatch batch monomorphization.
fn assert_logical_batch<const N: usize>() {
    let cases = [
        (Opcode::JumpIfFalse, Opcode::LoadFalse, None, None),
        (Opcode::JumpIfFalse, Opcode::LoadTrue, None, Some(42)),
        (Opcode::JumpIfTrue, Opcode::LoadFalse, None, Some(42)),
        (Opcode::JumpIfTrue, Opcode::LoadTrue, None, None),
        (Opcode::JumpIfNotNullish, Opcode::LoadNull, None, Some(42)),
        (
            Opcode::JumpIfNotNullish,
            Opcode::LoadImmediate,
            Some(7),
            Some(7),
        ),
    ];
    for (branch, left, immediate, expected_integer) in cases {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &logical_module(branch, left, immediate),
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        let RunOutcome::Completed(value) = outcome else {
            panic!("logical module must complete");
        };
        if let Some(expected_integer) = expected_integer {
            assert_eq!(value.as_i32(), Some(expected_integer));
        } else {
            let expected = if left == Opcode::LoadFalse {
                Immediate::False
            } else {
                Immediate::True
            };
            assert_eq!(value.as_immediate(), Some(expected));
        }
    }
}

fn assert_switch_batch<const N: usize>() {
    for (discriminant, expected) in [(1, 10), (2, 20), (9, 20)] {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &switch_module(discriminant),
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        assert!(
            matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(expected))
        );
    }
}

/// Builds a verified branch program with explicit labels to exercise PC changes inside one dispatch batch.
fn conditional_module(test: Opcode) -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(8, 2);
    let alternate = builder.new_label().unwrap();
    let end = builder.new_label().unwrap();
    builder.emit(test, &[0], span).unwrap();
    builder
        .emit_jump_if_false(RegisterId::new(0), alternate, span)
        .unwrap();
    builder.emit(Opcode::LoadImmediate, &[1, 1], span).unwrap();
    builder.emit_jump(end, span).unwrap();
    builder.bind_label(alternate).unwrap();
    builder.emit(Opcode::LoadImmediate, &[1, 2], span).unwrap();
    builder.bind_label(end).unwrap();
    builder.emit(Opcode::Return, &[1], span).unwrap();
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
        Arc::from("conditional"),
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

/// Builds a verified counting loop so every dispatch batch exercises a taken backward jump.
fn backedge_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(9, 2);
    let condition = builder.new_label().unwrap();
    let end = builder.new_label().unwrap();
    builder.emit(Opcode::LoadImmediate, &[0, 0], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[1, 3], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[2, 1], span).unwrap();
    builder.bind_label(condition).unwrap();
    builder.emit(Opcode::LessThan, &[3, 0, 1], span).unwrap();
    builder
        .emit_jump_if_false(RegisterId::new(3), end, span)
        .unwrap();
    builder.emit(Opcode::Add, &[0, 0, 2], span).unwrap();
    builder.emit_jump(condition, span).unwrap();
    builder.bind_label(end).unwrap();
    builder.emit(Opcode::Return, &[0], span).unwrap();
    single_function_module("backedge", Vec::new(), builder)
}

/// Builds two numeric own properties and counts the complete managed iterator snapshot.
fn for_in_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(24, 2);
    let condition = builder.new_label().unwrap();
    let end = builder.new_label().unwrap();
    builder.emit(Opcode::CreateObject, &[0], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[1, 0], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[2, 10], span).unwrap();
    builder.emit(Opcode::SetByValue, &[0, 2, 1], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[3, 1], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[4, 20], span).unwrap();
    builder.emit(Opcode::SetByValue, &[0, 4, 3], span).unwrap();
    builder
        .emit(Opcode::CreateForInIterator, &[5, 0], span)
        .unwrap();
    builder.emit(Opcode::LoadUndefined, &[6], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[7, 0], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[8, 1], span).unwrap();
    builder.bind_label(condition).unwrap();
    builder.emit(Opcode::ForInNext, &[9, 5], span).unwrap();
    builder
        .emit(Opcode::StrictEqual, &[10, 9, 6], span)
        .unwrap();
    builder
        .emit_jump_if_true(RegisterId::new(10), end, span)
        .unwrap();
    builder.emit(Opcode::Add, &[7, 7, 8], span).unwrap();
    builder.emit_jump(condition, span).unwrap();
    builder.bind_label(end).unwrap();
    builder.emit(Opcode::Return, &[7], span).unwrap();
    single_function_module("for-in", Vec::new(), builder)
}

/// Builds one operand-preserving short-circuit branch around a right-hand integer load.
fn logical_module(branch: Opcode, left: Opcode, immediate: Option<u32>) -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(6, 1);
    let end = builder.new_label().unwrap();
    if let Some(value) = immediate {
        builder.emit(left, &[0, value], span).unwrap();
    } else {
        builder.emit(left, &[0], span).unwrap();
    }
    builder.emit(Opcode::Move, &[1, 0], span).unwrap();
    match branch {
        Opcode::JumpIfFalse => builder.emit_jump_if_false(RegisterId::new(0), end, span),
        Opcode::JumpIfTrue => builder.emit_jump_if_true(RegisterId::new(0), end, span),
        Opcode::JumpIfNotNullish => builder.emit_jump_if_not_nullish(RegisterId::new(0), end, span),
        _ => panic!("test supplies a logical branch opcode"),
    }
    .unwrap();
    builder.emit(Opcode::LoadImmediate, &[2, 42], span).unwrap();
    builder.emit(Opcode::Move, &[1, 2], span).unwrap();
    builder.bind_label(end).unwrap();
    builder.emit(Opcode::Return, &[1], span).unwrap();
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
        Arc::from("logical"),
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

/// Builds a branch whose heap-string truthiness must leave and then resume the verified kernel.
#[cfg(feature = "opcode-profile")]
fn heap_string_branch_module(contents: Vec<u16>) -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(5, 1);
    let end = builder.new_label().unwrap();
    builder.emit(Opcode::LoadConstant, &[0, 0], span).unwrap();
    builder.emit(Opcode::Move, &[1, 0], span).unwrap();
    builder
        .emit_jump_if_false(RegisterId::new(0), end, span)
        .unwrap();
    builder.emit(Opcode::LoadImmediate, &[1, 42], span).unwrap();
    builder.bind_label(end).unwrap();
    builder.emit(Opcode::Return, &[1], span).unwrap();
    single_function_module(
        "heap string branch",
        vec![BytecodeConstant::string_from_utf16(contents)],
        builder,
    )
}

/// Builds a two-case dispatch whose middle default deliberately falls through into case two.
fn switch_module(discriminant: u32) -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(16, 4);
    let case_one = builder.new_label().unwrap();
    let default = builder.new_label().unwrap();
    let case_two = builder.new_label().unwrap();
    let end = builder.new_label().unwrap();
    builder
        .emit(Opcode::LoadImmediate, &[0, discriminant], span)
        .unwrap();
    builder.emit(Opcode::LoadImmediate, &[1, 1], span).unwrap();
    builder.emit(Opcode::StrictEqual, &[2, 0, 1], span).unwrap();
    builder
        .emit_jump_if_true(RegisterId::new(2), case_one, span)
        .unwrap();
    builder.emit(Opcode::LoadImmediate, &[3, 2], span).unwrap();
    builder.emit(Opcode::StrictEqual, &[4, 0, 3], span).unwrap();
    builder
        .emit_jump_if_true(RegisterId::new(4), case_two, span)
        .unwrap();
    builder.emit_jump(default, span).unwrap();
    builder.bind_label(case_one).unwrap();
    builder.emit(Opcode::LoadImmediate, &[5, 10], span).unwrap();
    builder.emit_jump(end, span).unwrap();
    builder.bind_label(default).unwrap();
    builder.emit(Opcode::LoadImmediate, &[5, 30], span).unwrap();
    builder.bind_label(case_two).unwrap();
    builder.emit(Opcode::LoadImmediate, &[5, 20], span).unwrap();
    builder.emit_jump(end, span).unwrap();
    builder.bind_label(end).unwrap();
    builder.emit(Opcode::Return, &[5], span).unwrap();
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
        Arc::from("switch"),
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

/// Builds two property additions followed by an allocation-free update and own read.
fn property_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(10, 0);
    builder.emit(Opcode::CreateObject, &[0], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[1, 41], span).unwrap();
    builder.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[2, 7], span).unwrap();
    builder.emit(Opcode::SetById, &[0, 2, 1], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[3, 42], span).unwrap();
    builder.emit(Opcode::SetById, &[0, 3, 0], span).unwrap();
    builder.emit(Opcode::GetById, &[4, 0, 0], span).unwrap();
    builder.emit(Opcode::Return, &[4], span).unwrap();
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
        Arc::from("properties"),
        Vec::new(),
        vec![Arc::from("answer"), Arc::from("other")],
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            bytecode,
            metadata,
        )],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds a numeric-key write/read pair over one ordinary object.
fn dynamic_property_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(6, 0);
    builder.emit(Opcode::CreateObject, &[0], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[1, 0], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[2, 42], span).unwrap();
    builder.emit(Opcode::SetByValue, &[0, 2, 1], span).unwrap();
    builder.emit(Opcode::GetByValue, &[3, 0, 1], span).unwrap();
    builder.emit(Opcode::Return, &[3], span).unwrap();
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
        Arc::from("dynamic properties"),
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

/// Builds a string-key write/read pair to cover the non-integer PropertyKey path.
fn dynamic_string_property_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(7, 1);
    builder.emit(Opcode::CreateObject, &[0], span).unwrap();
    builder.emit(Opcode::LoadConstant, &[1, 0], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[2, 42], span).unwrap();
    builder.emit(Opcode::SetByValue, &[0, 2, 1], span).unwrap();
    builder.emit(Opcode::GetByValue, &[3, 0, 1], span).unwrap();
    builder.emit(Opcode::Return, &[3], span).unwrap();
    single_function_module(
        "dynamic string property",
        vec![BytecodeConstant::string_from_utf16(
            "answer".encode_utf16().collect(),
        )],
        builder,
    )
}

/// Builds a numeric-key write/string-key read pair to verify ECMAScript formatting.
fn dynamic_numeric_property_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(7, 2);
    builder.emit(Opcode::CreateObject, &[0], span).unwrap();
    builder.emit(Opcode::LoadConstant, &[1, 0], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[2, 42], span).unwrap();
    builder.emit(Opcode::SetByValue, &[0, 2, 1], span).unwrap();
    builder.emit(Opcode::LoadConstant, &[3, 1], span).unwrap();
    builder.emit(Opcode::GetByValue, &[4, 0, 3], span).unwrap();
    builder.emit(Opcode::Return, &[4], span).unwrap();
    single_function_module(
        "dynamic numeric property",
        vec![
            BytecodeConstant::NumberBits(1.2f64.to_bits()),
            BytecodeConstant::string_from_utf16("1.2".encode_utf16().collect()),
        ],
        builder,
    )
}

/// Builds a callable carrying the same shape/storage path as an ordinary object.
fn function_property_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(6, 0);
    entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
    entry.emit(Opcode::LoadImmediate, &[1, 42], span).unwrap();
    entry.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
    entry.emit(Opcode::GetById, &[2, 0, 0], span).unwrap();
    entry.emit(Opcode::Return, &[2], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
    let mut callee = BytecodeBuilder::with_capacity(2, 0);
    callee.emit(Opcode::LoadUndefined, &[0], span).unwrap();
    callee.emit(Opcode::Return, &[0], span).unwrap();
    let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
    let entry_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: entry_registers,
            ..FunctionLayout::default()
        },
        source_map: entry_source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    let callee_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: callee_registers,
            ..FunctionLayout::default()
        },
        source_map: callee_source_map,
        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
    };
    CompiledModule::new(
        Arc::from("function property"),
        Vec::new(),
        vec![Arc::from("answer")],
        vec![
            CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
            CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds `identity.call(undefined, 42)` through the shared native Function prototype.
fn function_prototype_call_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(6, 0);
    entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
    entry.emit(Opcode::GetById, &[1, 0, 0], span).unwrap();
    entry.emit(Opcode::LoadUndefined, &[2], span).unwrap();
    entry.emit(Opcode::LoadImmediate, &[3, 42], span).unwrap();
    entry
        .emit(Opcode::CallWithReceiver, &[4, 0, 2], span)
        .unwrap();
    entry.emit(Opcode::Return, &[4], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
    let mut callee = BytecodeBuilder::with_capacity(1, 0);
    callee.emit(Opcode::Return, &[0], span).unwrap();
    let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
    let entry_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: entry_registers,
            ..FunctionLayout::default()
        },
        source_map: entry_source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    let callee_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: callee_registers,
            argument_count: 1,
            ..FunctionLayout::default()
        },
        source_map: callee_source_map,
        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
    };
    CompiledModule::new(
        Arc::from("identity.call(undefined, 42)"),
        Vec::new(),
        vec![Arc::from("call")],
        vec![
            CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
            CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds `(1.25).toFixed({ valueOf() { return 1; } }) === "1.3"` for trampoline tests.
fn number_continuation_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(13, 2);
    entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    entry.emit(Opcode::GetById, &[1, 0, 1], span).unwrap();
    entry.emit(Opcode::GetById, &[2, 1, 2], span).unwrap();
    entry.emit(Opcode::CreateObject, &[3], span).unwrap();
    entry.emit(Opcode::CreateClosure, &[4, 1], span).unwrap();
    entry.emit(Opcode::SetById, &[3, 4, 3], span).unwrap();
    entry.emit(Opcode::LoadConstant, &[5, 0], span).unwrap();
    entry.emit(Opcode::Move, &[6, 2], span).unwrap();
    entry.emit(Opcode::Move, &[7, 3], span).unwrap();
    entry
        .emit(Opcode::CallWithReceiver, &[8, 5, 1], span)
        .unwrap();
    entry.emit(Opcode::LoadConstant, &[9, 1], span).unwrap();
    entry.emit(Opcode::StrictEqual, &[10, 8, 9], span).unwrap();
    entry.emit(Opcode::Return, &[10], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
    let mut callback = BytecodeBuilder::with_capacity(6, 1);
    let protected_start = callback.emit(Opcode::Nop, &[], span).unwrap();
    callback
        .emit(Opcode::LoadImmediate, &[0, 42], span)
        .unwrap();
    callback.emit(Opcode::Throw, &[0], span).unwrap();
    let handler = callback.emit(Opcode::LoadException, &[1], span).unwrap();
    callback.emit(Opcode::LoadImmediate, &[2, 1], span).unwrap();
    callback.emit(Opcode::Return, &[2], span).unwrap();
    let (callback_bytecode, callback_source_map, callback_registers) = callback.finish().unwrap();
    let entry_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: entry_registers,
            ..FunctionLayout::default()
        },
        source_map: entry_source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    let mut callback_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: callback_registers,
            max_handler_depth: 1,
            ..FunctionLayout::default()
        },
        source_map: callback_source_map,
        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
    };
    callback_metadata.handlers = vec![HandlerEntry {
        protected_start,
        protected_end: handler,
        handler,
        kind: HandlerKind::Catch,
        environment_depth: 0,
    }]
    .into();
    CompiledModule::new(
        Arc::from("Number ToPrimitive continuation"),
        vec![
            BytecodeConstant::NumberBits(1.25_f64.to_bits()),
            BytecodeConstant::string_from_utf16("1.3".encode_utf16().collect()),
        ],
        vec![
            Arc::from("Number"),
            Arc::from("prototype"),
            Arc::from("toFixed"),
            Arc::from("valueOf"),
        ],
        vec![
            CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
            CompiledFunctionTemplate::new(FunctionId::new(1), callback_bytecode, callback_metadata),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds a protected Number call whose `valueOf` callback throws through its continuation.
fn number_continuation_throw_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(16, 2);
    let end = entry.new_label().unwrap();
    entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    entry.emit(Opcode::GetById, &[1, 0, 1], span).unwrap();
    entry.emit(Opcode::GetById, &[2, 1, 2], span).unwrap();
    entry.emit(Opcode::CreateObject, &[3], span).unwrap();
    entry.emit(Opcode::CreateClosure, &[4, 1], span).unwrap();
    entry.emit(Opcode::SetById, &[3, 4, 3], span).unwrap();
    entry.emit(Opcode::LoadConstant, &[5, 0], span).unwrap();
    entry.emit(Opcode::Move, &[6, 2], span).unwrap();
    entry.emit(Opcode::Move, &[7, 3], span).unwrap();
    let protected_start = entry.emit(Opcode::Nop, &[], span).unwrap();
    entry
        .emit(Opcode::CallWithReceiver, &[8, 5, 1], span)
        .unwrap();
    entry.emit_jump(end, span).unwrap();
    let handler = entry.emit(Opcode::LoadException, &[9], span).unwrap();
    entry.emit(Opcode::Return, &[9], span).unwrap();
    entry.bind_label(end).unwrap();
    entry.emit(Opcode::LoadUndefined, &[10], span).unwrap();
    entry.emit(Opcode::Return, &[10], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

    let mut callback = BytecodeBuilder::with_capacity(2, 0);
    callback
        .emit(Opcode::LoadImmediate, &[0, 42], span)
        .unwrap();
    callback.emit(Opcode::Throw, &[0], span).unwrap();
    let (callback_bytecode, callback_source_map, callback_registers) = callback.finish().unwrap();
    let mut entry_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: entry_registers,
            max_handler_depth: 1,
            ..FunctionLayout::default()
        },
        source_map: entry_source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    entry_metadata.handlers = vec![HandlerEntry {
        protected_start,
        protected_end: handler,
        handler,
        kind: HandlerKind::Catch,
        environment_depth: 0,
    }]
    .into();
    let callback_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: callback_registers,
            ..FunctionLayout::default()
        },
        source_map: callback_source_map,
        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
    };
    CompiledModule::new(
        Arc::from("Number continuation callback throw"),
        vec![BytecodeConstant::NumberBits(1.25_f64.to_bits())],
        vec![
            Arc::from("Number"),
            Arc::from("prototype"),
            Arc::from("toFixed"),
            Arc::from("valueOf"),
        ],
        vec![
            CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
            CompiledFunctionTemplate::new(FunctionId::new(1), callback_bytecode, callback_metadata),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds `String({ toString() { try/catch; return "converted"; } })` for trampoline tests.
fn string_continuation_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(8, 2);
    entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    entry.emit(Opcode::CreateObject, &[1], span).unwrap();
    entry.emit(Opcode::CreateClosure, &[2, 1], span).unwrap();
    entry.emit(Opcode::SetById, &[1, 2, 1], span).unwrap();
    entry.emit(Opcode::Call, &[3, 0, 1], span).unwrap();
    entry.emit(Opcode::LoadConstant, &[4, 0], span).unwrap();
    entry.emit(Opcode::StrictEqual, &[5, 3, 4], span).unwrap();
    entry.emit(Opcode::Return, &[5], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

    let mut callback = BytecodeBuilder::with_capacity(6, 1);
    let protected_start = callback.emit(Opcode::Nop, &[], span).unwrap();
    callback.emit(Opcode::LoadImmediate, &[0, 7], span).unwrap();
    callback.emit(Opcode::Throw, &[0], span).unwrap();
    let handler = callback.emit(Opcode::LoadException, &[1], span).unwrap();
    callback.emit(Opcode::LoadConstant, &[2, 0], span).unwrap();
    callback.emit(Opcode::Return, &[2], span).unwrap();
    let (callback_bytecode, callback_source_map, callback_registers) = callback.finish().unwrap();
    let entry_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: entry_registers,
            ..FunctionLayout::default()
        },
        source_map: entry_source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    let mut callback_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: callback_registers,
            max_handler_depth: 1,
            ..FunctionLayout::default()
        },
        source_map: callback_source_map,
        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
    };
    callback_metadata.handlers = vec![HandlerEntry {
        protected_start,
        protected_end: handler,
        handler,
        kind: HandlerKind::Catch,
        environment_depth: 0,
    }]
    .into();
    CompiledModule::new(
        Arc::from("String ToPrimitive continuation"),
        vec![BytecodeConstant::string_from_utf16(
            "converted".encode_utf16().collect(),
        )],
        vec![Arc::from("String"), Arc::from("toString")],
        vec![
            CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
            CompiledFunctionTemplate::new(FunctionId::new(1), callback_bytecode, callback_metadata),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds a callback-driven numeric unary expression and compares its exact result.
fn numeric_unary_continuation_module(opcode: Opcode, expected: i32) -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(7, 1);
    entry.emit(Opcode::CreateObject, &[0], span).unwrap();
    entry.emit(Opcode::CreateClosure, &[1, 1], span).unwrap();
    entry.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
    entry.emit(opcode, &[2, 0], span).unwrap();
    entry
        .emit(Opcode::LoadImmediate, &[3, expected as u32], span)
        .unwrap();
    entry.emit(Opcode::StrictEqual, &[4, 2, 3], span).unwrap();
    entry.emit(Opcode::Return, &[4], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

    let mut callback = BytecodeBuilder::with_capacity(6, 1);
    let protected_start = callback.emit(Opcode::Nop, &[], span).unwrap();
    callback.emit(Opcode::LoadImmediate, &[0, 8], span).unwrap();
    callback.emit(Opcode::Throw, &[0], span).unwrap();
    let handler = callback.emit(Opcode::LoadException, &[1], span).unwrap();
    callback.emit(Opcode::LoadImmediate, &[2, 7], span).unwrap();
    callback.emit(Opcode::Return, &[2], span).unwrap();
    let (callback_bytecode, callback_source_map, callback_registers) = callback.finish().unwrap();
    let entry_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: entry_registers,
            ..FunctionLayout::default()
        },
        source_map: entry_source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    let mut callback_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: callback_registers,
            max_handler_depth: 1,
            ..FunctionLayout::default()
        },
        source_map: callback_source_map,
        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
    };
    callback_metadata.handlers = vec![HandlerEntry {
        protected_start,
        protected_end: handler,
        handler,
        kind: HandlerKind::Catch,
        environment_depth: 0,
    }]
    .into();
    CompiledModule::new(
        Arc::from("numeric unary continuation"),
        Vec::new(),
        vec![Arc::from("valueOf")],
        vec![
            CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
            CompiledFunctionTemplate::new(FunctionId::new(1), callback_bytecode, callback_metadata),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds two callback-driven operands for an exact subtraction continuation result.
fn numeric_binary_continuation_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(10, 1);
    entry.emit(Opcode::CreateObject, &[0], span).unwrap();
    entry.emit(Opcode::CreateClosure, &[1, 1], span).unwrap();
    entry.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
    entry.emit(Opcode::CreateObject, &[2], span).unwrap();
    entry.emit(Opcode::CreateClosure, &[3, 2], span).unwrap();
    entry.emit(Opcode::SetById, &[2, 3, 0], span).unwrap();
    entry.emit(Opcode::Sub, &[4, 0, 2], span).unwrap();
    entry.emit(Opcode::LoadImmediate, &[5, 6], span).unwrap();
    entry.emit(Opcode::StrictEqual, &[6, 4, 5], span).unwrap();
    entry.emit(Opcode::Return, &[6], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
    let entry_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: entry_registers,
            ..FunctionLayout::default()
        },
        source_map: entry_source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    CompiledModule::new(
        Arc::from("numeric binary continuation"),
        Vec::new(),
        vec![Arc::from("valueOf")],
        vec![
            CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
            numeric_callback_template(FunctionId::new(1), 8, span),
            numeric_callback_template(FunctionId::new(2), 2, span),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds `left.valueOf() + right.valueOf() === "x2"` with a GC String left result.
fn add_continuation_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(10, 1);
    entry.emit(Opcode::CreateObject, &[0], span).unwrap();
    entry.emit(Opcode::CreateClosure, &[1, 1], span).unwrap();
    entry.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
    entry.emit(Opcode::CreateObject, &[2], span).unwrap();
    entry.emit(Opcode::CreateClosure, &[3, 2], span).unwrap();
    entry.emit(Opcode::SetById, &[2, 3, 0], span).unwrap();
    entry.emit(Opcode::Add, &[4, 0, 2], span).unwrap();
    entry.emit(Opcode::LoadConstant, &[5, 1], span).unwrap();
    entry.emit(Opcode::StrictEqual, &[6, 4, 5], span).unwrap();
    entry.emit(Opcode::Return, &[6], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
    let entry_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: entry_registers,
            ..FunctionLayout::default()
        },
        source_map: entry_source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    CompiledModule::new(
        Arc::from("Add continuation"),
        vec![
            BytecodeConstant::string_from_utf16("x".encode_utf16().collect()),
            BytecodeConstant::string_from_utf16("x2".encode_utf16().collect()),
        ],
        vec![Arc::from("valueOf")],
        vec![
            CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
            string_callback_template(FunctionId::new(1), 0, span),
            numeric_callback_template(FunctionId::new(2), 2, span),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds two string-returning callbacks for an exact relational continuation result.
fn relational_continuation_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(8, 1);
    entry.emit(Opcode::CreateObject, &[0], span).unwrap();
    entry.emit(Opcode::CreateClosure, &[1, 1], span).unwrap();
    entry.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
    entry.emit(Opcode::CreateObject, &[2], span).unwrap();
    entry.emit(Opcode::CreateClosure, &[3, 2], span).unwrap();
    entry.emit(Opcode::SetById, &[2, 3, 0], span).unwrap();
    entry.emit(Opcode::GreaterThan, &[4, 0, 2], span).unwrap();
    entry.emit(Opcode::Return, &[4], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
    let entry_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: entry_registers,
            ..FunctionLayout::default()
        },
        source_map: entry_source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    CompiledModule::new(
        Arc::from("relational continuation"),
        vec![
            BytecodeConstant::string_from_utf16("x".encode_utf16().collect()),
            BytecodeConstant::string_from_utf16("w".encode_utf16().collect()),
        ],
        vec![Arc::from("valueOf")],
        vec![
            CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
            string_callback_template(FunctionId::new(1), 0, span),
            string_callback_template(FunctionId::new(2), 1, span),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds a callback with an internal throw/catch before returning one numeric primitive.
fn numeric_callback_template(
    function: FunctionId,
    returned: i32,
    span: SourceSpan,
) -> CompiledFunctionTemplate {
    let mut callback = BytecodeBuilder::with_capacity(6, 1);
    let protected_start = callback.emit(Opcode::Nop, &[], span).unwrap();
    callback.emit(Opcode::LoadImmediate, &[0, 1], span).unwrap();
    callback.emit(Opcode::Throw, &[0], span).unwrap();
    let handler = callback.emit(Opcode::LoadException, &[1], span).unwrap();
    callback
        .emit(Opcode::LoadImmediate, &[2, returned as u32], span)
        .unwrap();
    callback.emit(Opcode::Return, &[2], span).unwrap();
    let (bytecode, source_map, register_count) = callback.finish().unwrap();
    let mut metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count,
            max_handler_depth: 1,
            ..FunctionLayout::default()
        },
        source_map,
        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
    };
    metadata.handlers = vec![HandlerEntry {
        protected_start,
        protected_end: handler,
        handler,
        kind: HandlerKind::Catch,
        environment_depth: 0,
    }]
    .into();
    CompiledFunctionTemplate::new(function, bytecode, metadata)
}

/// Builds a callback with an internal catch before returning one string constant.
fn string_callback_template(
    function: FunctionId,
    constant: u32,
    span: SourceSpan,
) -> CompiledFunctionTemplate {
    let mut callback = BytecodeBuilder::with_capacity(6, 1);
    let protected_start = callback.emit(Opcode::Nop, &[], span).unwrap();
    callback.emit(Opcode::LoadImmediate, &[0, 1], span).unwrap();
    callback.emit(Opcode::Throw, &[0], span).unwrap();
    let handler = callback.emit(Opcode::LoadException, &[1], span).unwrap();
    callback
        .emit(Opcode::LoadConstant, &[2, constant], span)
        .unwrap();
    callback.emit(Opcode::Return, &[2], span).unwrap();
    let (bytecode, source_map, register_count) = callback.finish().unwrap();
    let mut metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count,
            max_handler_depth: 1,
            ..FunctionLayout::default()
        },
        source_map,
        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
    };
    metadata.handlers = vec![HandlerEntry {
        protected_start,
        protected_end: handler,
        handler,
        kind: HandlerKind::Catch,
        environment_depth: 0,
    }]
    .into();
    CompiledFunctionTemplate::new(function, bytecode, metadata)
}

/// Builds `add.bind(undefined, 20)(22)` with one immutable bound-argument prefix.
fn bound_function_call_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(8, 0);
    entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
    entry.emit(Opcode::GetById, &[1, 0, 0], span).unwrap();
    entry.emit(Opcode::LoadUndefined, &[2], span).unwrap();
    entry.emit(Opcode::LoadImmediate, &[3, 20], span).unwrap();
    entry
        .emit(Opcode::CallWithReceiver, &[4, 0, 2], span)
        .unwrap();
    entry.emit(Opcode::LoadImmediate, &[5, 22], span).unwrap();
    entry.emit(Opcode::Call, &[6, 4, 1], span).unwrap();
    entry.emit(Opcode::Return, &[6], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

    let mut callee = BytecodeBuilder::with_capacity(2, 0);
    callee.emit(Opcode::Add, &[2, 0, 1], span).unwrap();
    callee.emit(Opcode::Return, &[2], span).unwrap();
    let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
    CompiledModule::new(
        Arc::from("add.bind(undefined, 20)(22)"),
        Vec::new(),
        vec![Arc::from("bind"), Arc::from("add")],
        vec![
            CompiledFunctionTemplate::new(
                FunctionId::new(0),
                entry_bytecode,
                FunctionMetadata {
                    layout: FunctionLayout {
                        register_count: entry_registers,
                        ..FunctionLayout::default()
                    },
                    source_map: entry_source_map,
                    ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
                },
            ),
            CompiledFunctionTemplate::new(
                FunctionId::new(1),
                callee_bytecode,
                FunctionMetadata {
                    layout: FunctionLayout {
                        register_count: callee_registers,
                        argument_count: 2,
                        function_length: 2,
                        name_scope: Some(1),
                        ..FunctionLayout::default()
                    },
                    source_map: callee_source_map,
                    ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
                },
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds `[].push(20, 22)` through a verified receiver/callee/argument window.
fn array_push_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(6, 0);
    builder.emit(Opcode::CreateArray, &[0], span).unwrap();
    builder.emit(Opcode::GetById, &[1, 0, 0], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[2, 20], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[3, 22], span).unwrap();
    builder
        .emit(Opcode::CallWithReceiver, &[4, 0, 2], span)
        .unwrap();
    builder.emit(Opcode::Return, &[4], span).unwrap();
    let (bytecode, source_map, register_count) = builder.finish().unwrap();
    CompiledModule::new(
        Arc::from("[].push(20, 22)"),
        Vec::new(),
        vec![Arc::from("push")],
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

/// Builds `readThis.call(undefined)` with caller-selected immutable function strictness.
fn this_binding_module(strictness: FunctionStrictness) -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(5, 0);
    entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
    entry.emit(Opcode::GetById, &[1, 0, 0], span).unwrap();
    entry.emit(Opcode::LoadUndefined, &[2], span).unwrap();
    entry
        .emit(Opcode::CallWithReceiver, &[3, 0, 1], span)
        .unwrap();
    entry.emit(Opcode::Return, &[3], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
    let mut callee = BytecodeBuilder::with_capacity(2, 0);
    callee.emit(Opcode::LoadThis, &[0], span).unwrap();
    callee.emit(Opcode::Return, &[0], span).unwrap();
    let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
    let entry_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: entry_registers,
            ..FunctionLayout::default()
        },
        source_map: entry_source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    let callee_metadata = FunctionMetadata {
        strictness,
        layout: FunctionLayout {
            register_count: callee_registers,
            ..FunctionLayout::default()
        },
        source_map: callee_source_map,
        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
    };
    CompiledModule::new(
        Arc::from("readThis.call(undefined)"),
        Vec::new(),
        vec![Arc::from("call")],
        vec![
            CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
            CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds one unresolved assignment with caller-selected strict throw or sloppy publication.
fn unresolved_assignment_module(strictness: FunctionStrictness) -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(4, 0);
    builder.emit(Opcode::LoadImmediate, &[0, 42], span).unwrap();
    builder
        .emit(Opcode::StoreResolvedScope, &[0, 0], span)
        .unwrap();
    builder.emit(Opcode::LoadScope, &[1, 0], span).unwrap();
    builder.emit(Opcode::Return, &[1], span).unwrap();
    let (bytecode, source_map, register_count) = builder.finish().unwrap();
    let metadata = FunctionMetadata {
        strictness,
        layout: FunctionLayout {
            register_count,
            ..FunctionLayout::default()
        },
        source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    CompiledModule::new(
        Arc::from("missing = 42; missing"),
        Vec::new(),
        vec![Arc::from("missing")],
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            bytecode,
            metadata,
        )],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds `new ReferenceError() instanceof ReferenceError` over native construct dispatch.
fn native_error_constructor_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(4, 0);
    builder.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    builder.emit(Opcode::Construct, &[1, 0, 0], span).unwrap();
    builder.emit(Opcode::InstanceOf, &[2, 1, 0], span).unwrap();
    builder.emit(Opcode::Return, &[2], span).unwrap();
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
        Arc::from("new ReferenceError() instanceof ReferenceError"),
        Vec::new(),
        vec![Arc::from("ReferenceError")],
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            bytecode,
            metadata,
        )],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds a method call that stops immediately after pushing its receiver-bearing frame.
fn method_receiver_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(8, 0);
    entry.emit(Opcode::CreateObject, &[0], span).unwrap();
    entry.emit(Opcode::CreateClosure, &[1, 1], span).unwrap();
    entry.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
    entry.emit(Opcode::Move, &[2, 0], span).unwrap();
    entry.emit(Opcode::GetById, &[3, 2, 0], span).unwrap();
    entry
        .emit(Opcode::CallWithReceiver, &[4, 2, 0], span)
        .unwrap();
    entry.emit(Opcode::Return, &[4], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
    let mut callee = BytecodeBuilder::with_capacity(2, 0);
    callee.emit(Opcode::LoadUndefined, &[0], span).unwrap();
    callee.emit(Opcode::Return, &[0], span).unwrap();
    let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
    let entry_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: entry_registers,
            ..FunctionLayout::default()
        },
        source_map: entry_source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    let callee_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: callee_registers,
            ..FunctionLayout::default()
        },
        source_map: callee_source_map,
        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
    };
    CompiledModule::new(
        Arc::from("method receiver"),
        Vec::new(),
        vec![Arc::from("method")],
        vec![
            CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
            CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Stores a young object through an allocation that forces root tracing before publication.
fn heap_value_property_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(6, 0);
    builder.emit(Opcode::CreateObject, &[0], span).unwrap();
    builder.emit(Opcode::CreateObject, &[1], span).unwrap();
    builder.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
    builder.emit(Opcode::GetById, &[2, 0, 0], span).unwrap();
    builder.emit(Opcode::Return, &[2], span).unwrap();
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
        Arc::from("heap value property"),
        Vec::new(),
        vec![Arc::from("child")],
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            bytecode,
            metadata,
        )],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Stores a young object through a callable's embedded ordinary-property edge.
fn function_heap_value_property_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(6, 0);
    entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
    entry.emit(Opcode::CreateObject, &[1], span).unwrap();
    entry.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
    entry.emit(Opcode::GetById, &[2, 0, 0], span).unwrap();
    entry.emit(Opcode::Return, &[2], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
    let mut callee = BytecodeBuilder::with_capacity(2, 0);
    callee.emit(Opcode::LoadUndefined, &[0], span).unwrap();
    callee.emit(Opcode::Return, &[0], span).unwrap();
    let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
    let entry_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: entry_registers,
            ..FunctionLayout::default()
        },
        source_map: entry_source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    let callee_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: callee_registers,
            ..FunctionLayout::default()
        },
        source_map: callee_source_map,
        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
    };
    CompiledModule::new(
        Arc::from("function heap property"),
        Vec::new(),
        vec![Arc::from("child")],
        vec![
            CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
            CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds a same-frame catch range with a pending-exception load at its handler target.
fn direct_catch_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(10, 1);
    let end = builder.new_label().unwrap();
    let protected_start = builder.emit(Opcode::Nop, &[], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[0, 42], span).unwrap();
    builder.emit(Opcode::Throw, &[0], span).unwrap();
    builder.emit_jump(end, span).unwrap();
    let handler = builder.emit(Opcode::LoadException, &[1], span).unwrap();
    builder.emit(Opcode::Return, &[1], span).unwrap();
    builder.bind_label(end).unwrap();
    builder.emit(Opcode::LoadUndefined, &[2], span).unwrap();
    builder.emit(Opcode::Return, &[2], span).unwrap();
    let (bytecode, source_map, register_count) = builder.finish().unwrap();
    let mut metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count,
            max_handler_depth: 1,
            ..FunctionLayout::default()
        },
        source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    metadata.handlers = vec![HandlerEntry {
        protected_start,
        protected_end: handler,
        handler,
        kind: HandlerKind::Catch,
        environment_depth: 0,
    }]
    .into();
    CompiledModule::new(
        Arc::from("direct catch"),
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

/// Builds a caller handler around a call whose callee throws through its explicit frame.
fn cross_frame_catch_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(12, 1);
    let end = entry.new_label().unwrap();
    entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
    let protected_start = entry.emit(Opcode::Nop, &[], span).unwrap();
    entry.emit(Opcode::Call, &[1, 0, 0], span).unwrap();
    entry.emit_jump(end, span).unwrap();
    let handler = entry.emit(Opcode::LoadException, &[2], span).unwrap();
    entry.emit(Opcode::Return, &[2], span).unwrap();
    entry.bind_label(end).unwrap();
    entry.emit(Opcode::LoadUndefined, &[3], span).unwrap();
    entry.emit(Opcode::Return, &[3], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
    let mut callee = BytecodeBuilder::with_capacity(2, 0);
    callee.emit(Opcode::LoadImmediate, &[0, 42], span).unwrap();
    callee.emit(Opcode::Throw, &[0], span).unwrap();
    let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
    let mut entry_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: entry_registers,
            max_handler_depth: 1,
            ..FunctionLayout::default()
        },
        source_map: entry_source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    entry_metadata.handlers = vec![HandlerEntry {
        protected_start,
        protected_end: handler,
        handler,
        kind: HandlerKind::Catch,
        environment_depth: 0,
    }]
    .into();
    let callee_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: callee_registers,
            ..FunctionLayout::default()
        },
        source_map: callee_source_map,
        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
    };
    CompiledModule::new(
        Arc::from("cross-frame catch"),
        Vec::new(),
        Vec::new(),
        vec![
            CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
            CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds a constructor that stores one argument on `this` and returns a primitive fallback.
fn construct_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(6, 0);
    entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
    entry.emit(Opcode::LoadImmediate, &[1, 42], span).unwrap();
    entry.emit(Opcode::Construct, &[2, 0, 1], span).unwrap();
    entry.emit(Opcode::GetById, &[3, 2, 0], span).unwrap();
    entry.emit(Opcode::Return, &[3], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
    let mut constructor = BytecodeBuilder::with_capacity(6, 0);
    constructor.emit(Opcode::LoadThis, &[1], span).unwrap();
    constructor.emit(Opcode::SetById, &[1, 0, 0], span).unwrap();
    constructor
        .emit(Opcode::LoadImmediate, &[2, 7], span)
        .unwrap();
    constructor.emit(Opcode::Return, &[2], span).unwrap();
    let (constructor_bytecode, constructor_source_map, constructor_registers) =
        constructor.finish().unwrap();
    let entry_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: entry_registers,
            ..FunctionLayout::default()
        },
        source_map: entry_source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    let constructor_metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count: constructor_registers,
            argument_count: 1,
            ..FunctionLayout::default()
        },
        source_map: constructor_source_map,
        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
    };
    CompiledModule::new(
        Arc::from("construct"),
        Vec::new(),
        vec![Arc::from("value")],
        vec![
            CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
            CompiledFunctionTemplate::new(
                FunctionId::new(1),
                constructor_bytecode,
                constructor_metadata,
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds one default constructor, constructs its receiver, then checks the real prototype link.
fn instanceof_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut entry = BytecodeBuilder::with_capacity(4, 0);
    entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
    entry.emit(Opcode::Construct, &[1, 0, 0], span).unwrap();
    entry.emit(Opcode::InstanceOf, &[2, 1, 0], span).unwrap();
    entry.emit(Opcode::Return, &[2], span).unwrap();
    let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
    let mut constructor = BytecodeBuilder::with_capacity(1, 0);
    constructor
        .emit(Opcode::ReturnUndefined, &[], span)
        .unwrap();
    let (constructor_bytecode, constructor_source_map, constructor_registers) =
        constructor.finish().unwrap();
    CompiledModule::new(
        Arc::from("new Constructor() instanceof Constructor"),
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
                    source_map: entry_source_map,
                    ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
                },
            ),
            CompiledFunctionTemplate::new(
                FunctionId::new(1),
                constructor_bytecode,
                FunctionMetadata {
                    layout: FunctionLayout {
                        register_count: constructor_registers,
                        ..FunctionLayout::default()
                    },
                    source_map: constructor_source_map,
                    ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
                },
            ),
        ],
        FunctionId::new(0),
    )
    .unwrap()
}

/// Builds the smallest executable module carrying a chosen entry-state layout contract.
fn state_module(kind: FunctionKind, layout: FunctionLayout) -> CompiledModule {
    let mut words = encode_instruction(Opcode::LoadUndefined, &[0]).unwrap();
    words.extend(encode_instruction(Opcode::Return, &[0]).unwrap());
    CompiledModule::new(
        Arc::from("state"),
        Vec::new(),
        Vec::new(),
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            Bytecode::from_words(words),
            FunctionMetadata::new(kind, layout),
        )],
        FunctionId::new(0),
    )
    .unwrap()
}
