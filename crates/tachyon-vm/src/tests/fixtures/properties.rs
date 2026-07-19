use super::super::*;
use super::*;

/// Builds two property additions followed by an allocation-free update and own read.
pub(in crate::tests) fn property_module() -> CompiledModule {
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
pub(in crate::tests) fn dynamic_property_module() -> CompiledModule {
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
pub(in crate::tests) fn dynamic_string_property_module() -> CompiledModule {
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
pub(in crate::tests) fn dynamic_numeric_property_module() -> CompiledModule {
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
pub(in crate::tests) fn function_property_module() -> CompiledModule {
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

/// Builds a method call that stops immediately after pushing its receiver-bearing frame.
pub(in crate::tests) fn method_receiver_module() -> CompiledModule {
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
pub(in crate::tests) fn heap_value_property_module() -> CompiledModule {
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
pub(in crate::tests) fn function_heap_value_property_module() -> CompiledModule {
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
