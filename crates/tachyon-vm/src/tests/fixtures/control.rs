use super::super::*;
use super::*;

/// Builds a verified branch program with explicit labels to exercise PC changes inside one dispatch batch.
pub(in crate::tests) fn conditional_module(test: Opcode) -> CompiledModule {
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
pub(in crate::tests) fn backedge_module() -> CompiledModule {
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
pub(in crate::tests) fn for_in_module() -> CompiledModule {
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
pub(in crate::tests) fn logical_module(
    branch: Opcode,
    left: Opcode,
    immediate: Option<u32>,
) -> CompiledModule {
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
pub(in crate::tests) fn heap_string_branch_module(contents: Vec<u16>) -> CompiledModule {
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
pub(in crate::tests) fn switch_module(discriminant: u32) -> CompiledModule {
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
