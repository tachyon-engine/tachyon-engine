use super::super::*;

/// Builds `identity.call(undefined, 42)` through the shared native Function prototype.
pub(in crate::tests) fn function_prototype_call_module() -> CompiledModule {
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
pub(in crate::tests) fn number_continuation_module() -> CompiledModule {
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
        handler_end: handler,
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
pub(in crate::tests) fn number_continuation_throw_module() -> CompiledModule {
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
        handler_end: handler,
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
pub(in crate::tests) fn string_continuation_module() -> CompiledModule {
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
        handler_end: handler,
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
pub(in crate::tests) fn numeric_unary_continuation_module(
    opcode: Opcode,
    expected: i32,
) -> CompiledModule {
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
        handler_end: handler,
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
pub(in crate::tests) fn numeric_binary_continuation_module() -> CompiledModule {
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
pub(in crate::tests) fn add_continuation_module() -> CompiledModule {
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
pub(in crate::tests) fn relational_continuation_module() -> CompiledModule {
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
pub(in crate::tests) fn numeric_callback_template(
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
        handler_end: handler,
        kind: HandlerKind::Catch,
        environment_depth: 0,
    }]
    .into();
    CompiledFunctionTemplate::new(function, bytecode, metadata)
}

/// Builds a callback with an internal catch before returning one string constant.
pub(in crate::tests) fn string_callback_template(
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
        handler_end: handler,
        kind: HandlerKind::Catch,
        environment_depth: 0,
    }]
    .into();
    CompiledFunctionTemplate::new(function, bytecode, metadata)
}

/// Builds `add.bind(undefined, 20)(22)` with one immutable bound-argument prefix.
pub(in crate::tests) fn bound_function_call_module() -> CompiledModule {
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
pub(in crate::tests) fn array_push_module() -> CompiledModule {
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
pub(in crate::tests) fn this_binding_module(strictness: FunctionStrictness) -> CompiledModule {
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
pub(in crate::tests) fn unresolved_assignment_module(
    strictness: FunctionStrictness,
) -> CompiledModule {
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
pub(in crate::tests) fn native_error_constructor_module() -> CompiledModule {
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

/// Builds a same-frame catch range with a pending-exception load at its handler target.
pub(in crate::tests) fn direct_catch_module() -> CompiledModule {
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
        handler_end: handler,
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
pub(in crate::tests) fn cross_frame_catch_module() -> CompiledModule {
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
        handler_end: handler,
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
pub(in crate::tests) fn construct_module() -> CompiledModule {
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
pub(in crate::tests) fn instanceof_module() -> CompiledModule {
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
pub(in crate::tests) fn state_module(kind: FunctionKind, layout: FunctionLayout) -> CompiledModule {
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
