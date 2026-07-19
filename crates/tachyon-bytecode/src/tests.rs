use super::*;
use proptest::prelude::*;
fn context() -> VerifyContext {
    VerifyContext {
        register_count: 4,
        constant_count: 2,
        function_count: 1,
        scope_name_count: 1,
        max_environment_slot_count: 2,
    }
}
#[test]
fn encoding_selects_all_widths() {
    assert_eq!(
        decode_instruction(
            &encode_instruction(Opcode::Add, &[1, 2, 3]).unwrap(),
            WordOffset::new(0)
        )
        .unwrap()
        .width,
        OperandWidth::Compact
    );
    assert_eq!(
        decode_instruction(
            &encode_instruction(Opcode::Add, &[256, 2, 3]).unwrap(),
            WordOffset::new(0)
        )
        .unwrap()
        .width,
        OperandWidth::Normal
    );
    assert_eq!(
        decode_instruction(
            &encode_instruction(Opcode::Add, &[65_536, 2, 3]).unwrap(),
            WordOffset::new(0)
        )
        .unwrap()
        .width,
        OperandWidth::Wide
    );
}

#[test]
/// Preserves the former exhaustive match as an independent oracle for all semantic opcodes.
fn operand_count_table_covers_every_opcode_once() {
    let groups: &[(usize, &[Opcode])] = &[
        (
            0,
            &[
                Opcode::Nop,
                Opcode::ReturnUndefined,
                Opcode::EnterFinally,
                Opcode::ResumeCompletion,
            ],
        ),
        (
            1,
            &[
                Opcode::LoadUndefined,
                Opcode::LoadNull,
                Opcode::LoadFalse,
                Opcode::LoadTrue,
                Opcode::Jump,
                Opcode::Return,
                Opcode::Throw,
                Opcode::DeclareScope,
                Opcode::CreateObject,
                Opcode::CreateArray,
                Opcode::LoadException,
                Opcode::LoadThis,
                Opcode::LoadNewTarget,
                Opcode::LoadArgumentsLength,
                Opcode::BreakThroughFinally,
                Opcode::ContinueThroughFinally,
            ],
        ),
        (
            2,
            &[
                Opcode::LoadImmediate,
                Opcode::LoadConstant,
                Opcode::Move,
                Opcode::Not,
                Opcode::Negate,
                Opcode::JumpIfFalse,
                Opcode::JumpIfTrue,
                Opcode::JumpIfNotNullish,
                Opcode::CreateClosure,
                Opcode::LoadScope,
                Opcode::StoreScope,
                Opcode::StoreResolvedScope,
                Opcode::DeclareGlobalLexical,
                Opcode::InitializeGlobalLexical,
                Opcode::Typeof,
                Opcode::ToNumber,
                Opcode::BitwiseNot,
                Opcode::CreateForInIterator,
                Opcode::ForInNext,
                Opcode::TypeofScope,
            ],
        ),
        (
            3,
            &[
                Opcode::Add,
                Opcode::Sub,
                Opcode::Mul,
                Opcode::Div,
                Opcode::StrictEqual,
                Opcode::LessThan,
                Opcode::InstanceOf,
                Opcode::GetByValue,
                Opcode::SetByValue,
                Opcode::LoadEnvironment,
                Opcode::StoreEnvironment,
                Opcode::Call,
                Opcode::Await,
                Opcode::Yield,
                Opcode::BitwiseAnd,
                Opcode::BitwiseOr,
                Opcode::BitwiseXor,
                Opcode::ShiftLeft,
                Opcode::ShiftRight,
                Opcode::ShiftRightUnsigned,
                Opcode::Remainder,
                Opcode::Exponentiate,
                Opcode::GreaterThan,
                Opcode::LessEqual,
                Opcode::GreaterEqual,
                Opcode::LooseEqual,
                Opcode::LooseNotEqual,
                Opcode::HasProperty,
                Opcode::DeleteById,
                Opcode::DeleteByValue,
                Opcode::GetById,
                Opcode::SetById,
                Opcode::CallWithReceiver,
                Opcode::Construct,
            ],
        ),
    ];
    let mut seen = [false; OPCODE_COUNT];
    let mut visited = 0;
    for &(expected_count, opcodes) in groups {
        for &opcode in opcodes {
            let index = opcode as usize;
            assert!(index < OPCODE_COUNT);
            assert!(!seen[index], "duplicate opcode {opcode:?}");
            seen[index] = true;
            visited += 1;
            assert_eq!(opcode.operand_count(), expected_count, "{opcode:?}");
        }
    }
    assert_eq!(visited, OPCODE_COUNT);
    assert!(seen.into_iter().all(|entry| entry));
}

#[test]
fn dense_opcode_indices_round_trip_without_holes() {
    for index in 0..Opcode::COUNT {
        let opcode = Opcode::from_index(index).expect("every dense index names an opcode");
        assert_eq!(opcode as usize, index);
    }
    assert_eq!(Opcode::from_index(Opcode::COUNT), None);
    assert_eq!(Opcode::from_index(usize::MAX), None);
}

#[test]
/// Cross-checks every base width and operand count plus all extended operand counts.
fn verified_decoder_matches_checked_decoder_at_stream_boundaries() {
    let mut words = Vec::new();
    let mut cases = Vec::new();
    let base_cases = [
        (
            Opcode::Nop,
            [
                encode_instruction(Opcode::Nop, &[]).unwrap(),
                vec![u32::from(Opcode::Nop as u8 | NORMAL_FORMAT)],
                vec![u32::from(Opcode::Nop as u8 | WIDE_FORMAT)],
            ],
        ),
        (
            Opcode::LoadUndefined,
            [
                encode_instruction(Opcode::LoadUndefined, &[1]).unwrap(),
                encode_instruction(Opcode::LoadUndefined, &[256]).unwrap(),
                encode_instruction(Opcode::LoadUndefined, &[65_536]).unwrap(),
            ],
        ),
        (
            Opcode::LoadImmediate,
            [
                encode_instruction(Opcode::LoadImmediate, &[0, 1]).unwrap(),
                encode_instruction(Opcode::LoadImmediate, &[0, 256]).unwrap(),
                encode_instruction(Opcode::LoadImmediate, &[0, 65_536]).unwrap(),
            ],
        ),
        (
            Opcode::Add,
            [
                encode_instruction(Opcode::Add, &[0, 1, 2]).unwrap(),
                encode_instruction(Opcode::Add, &[0, 1, 256]).unwrap(),
                encode_instruction(Opcode::Add, &[0, 1, 65_536]).unwrap(),
            ],
        ),
    ];
    for (opcode, encodings) in base_cases {
        for (encoding, width) in encodings.into_iter().zip([
            OperandWidth::Compact,
            OperandWidth::Normal,
            OperandWidth::Wide,
        ]) {
            append_decoder_case(&mut words, &mut cases, encoding, opcode, width);
        }
    }
    for (opcode, operands) in [
        (Opcode::CreateArray, &[1][..]),
        (Opcode::CreateForInIterator, &[2, 1][..]),
        (Opcode::DeleteById, &[2, 1, 0][..]),
    ] {
        append_decoder_case(
            &mut words,
            &mut cases,
            encode_instruction(opcode, operands).unwrap(),
            opcode,
            OperandWidth::Wide,
        );
    }
    append_decoder_case(
        &mut words,
        &mut cases,
        encode_instruction(Opcode::Return, &[65_536]).unwrap(),
        Opcode::Return,
        OperandWidth::Wide,
    );

    let verified = Bytecode::from_words(words)
        .verify(VerifyContext {
            register_count: 65_537,
            ..context()
        })
        .unwrap();
    let decoder = VerifiedInstructionDecoder::new(&verified);
    let checked_words = verified.bytecode().words();
    assert_eq!(cases.first().unwrap().0, WordOffset::new(0));

    for &(offset, expected_opcode, expected_width) in &cases {
        assert!(verified.is_instruction_start(offset));
        let checked = decode_instruction(checked_words, offset).unwrap();
        // SAFETY: every recorded offset was appended as one complete instruction and the whole
        // stream was accepted by the exact `VerifiedBytecode` borrowed by `decoder`.
        let fast = unsafe { decoder.decode_unchecked(offset) };
        assert_eq!(fast, checked);
        assert_eq!(fast.opcode, expected_opcode);
        assert_eq!(fast.width, expected_width);
    }

    let last_offset = cases.last().unwrap().0;
    let last = decode_instruction(checked_words, last_offset).unwrap();
    assert_eq!(last.opcode, Opcode::Return);
    assert_eq!(last.width, OperandWidth::Wide);
    assert_eq!(
        last_offset.index() as usize + usize::from(last.word_len),
        checked_words.len()
    );
}

fn append_decoder_case(
    words: &mut Vec<u32>,
    cases: &mut Vec<(WordOffset, Opcode, OperandWidth)>,
    encoding: Vec<u32>,
    opcode: Opcode,
    width: OperandWidth,
) {
    cases.push((WordOffset::new(words.len() as u32), opcode, width));
    words.extend(encoding);
}

#[test]
fn extended_opcode_escape_roundtrips_full_operands() {
    let words = encode_instruction(Opcode::DeleteById, &[7, 8, 9]).unwrap();
    assert_eq!(words[0] as u8 & FORMAT_MASK, ESCAPE_FORMAT);
    let decoded = decode_instruction(&words, WordOffset::new(0)).unwrap();
    assert_eq!(decoded.opcode, Opcode::DeleteById);
    assert_eq!(decoded.operands, [7, 8, 9]);
    assert_eq!(decoded.word_len, 4);

    let words = encode_instruction(Opcode::CreateArray, &[u32::MAX]).unwrap();
    let decoded = decode_instruction(&words, WordOffset::new(0)).unwrap();
    assert_eq!(decoded.opcode, Opcode::CreateArray);
    assert_eq!(decoded.operand(0), Some(u32::MAX));
    assert_eq!(decoded.word_len, 2);

    for opcode in [Opcode::CreateForInIterator, Opcode::ForInNext] {
        let words = encode_instruction(opcode, &[17, 23]).unwrap();
        let decoded = decode_instruction(&words, WordOffset::new(0)).unwrap();
        assert_eq!(decoded.opcode, opcode);
        assert_eq!(decoded.operands, [17, 23, 0]);
    }
}
#[test]
fn verifier_rejects_operand_word_jump_target() {
    let mut words = encode_instruction(Opcode::LoadImmediate, &[0, 256]).unwrap();
    words.extend(encode_instruction(Opcode::Jump, &[1]).unwrap());
    let error = Bytecode::from_words(words).verify(context()).unwrap_err();
    assert!(matches!(error, VerifyError::InvalidJumpTarget { .. }));
}

#[test]
fn verifier_rejects_conditional_branches_into_operand_words() {
    for opcode in [
        Opcode::JumpIfFalse,
        Opcode::JumpIfTrue,
        Opcode::JumpIfNotNullish,
    ] {
        let mut words = encode_instruction(Opcode::LoadImmediate, &[0, 256]).unwrap();
        words.extend(encode_instruction(opcode, &[0, 1]).unwrap());
        words.extend(encode_instruction(Opcode::Return, &[0]).unwrap());
        let error = Bytecode::from_words(words).verify(context()).unwrap_err();
        assert!(matches!(error, VerifyError::InvalidJumpTarget { .. }));
    }
}
#[test]
fn verifier_accepts_simple_terminal_program() {
    let mut words = encode_instruction(Opcode::LoadImmediate, &[0, 1]).unwrap();
    words.extend(encode_instruction(Opcode::Return, &[0]).unwrap());
    assert!(Bytecode::from_words(words).verify(context()).is_ok());
}

#[test]
fn verifier_rejects_call_argument_window_past_register_file() {
    let mut words = encode_instruction(Opcode::Call, &[0, 2, 2]).unwrap();
    words.extend(encode_instruction(Opcode::Return, &[0]).unwrap());
    assert!(matches!(
        Bytecode::from_words(words).verify(context()),
        Err(VerifyError::InvalidCallArgumentWindow {
            callee: 2,
            argument_count: 2,
            register_count: 4,
            ..
        })
    ));
}

#[test]
fn verifier_rejects_scope_name_index_past_module_table() {
    let mut words = encode_instruction(Opcode::LoadScope, &[0, 1]).unwrap();
    words.extend(encode_instruction(Opcode::Return, &[0]).unwrap());
    assert!(matches!(
        Bytecode::from_words(words).verify(context()),
        Err(VerifyError::ScopeNameOutOfRange {
            scope_name: 1,
            scope_name_count: 1,
            ..
        })
    ));
}

#[test]
fn verifier_rejects_environment_slot_past_module_maximum() {
    let mut words = encode_instruction(Opcode::LoadEnvironment, &[0, 1, 2]).unwrap();
    words.extend(encode_instruction(Opcode::Return, &[0]).unwrap());
    assert!(matches!(
        Bytecode::from_words(words).verify(context()),
        Err(VerifyError::EnvironmentSlotOutOfRange {
            slot: 2,
            max_environment_slot_count: 2,
            ..
        })
    ));
}

#[test]
fn verifier_rejects_non_boolean_global_lexical_mutability() {
    let mut words = encode_instruction(Opcode::DeclareGlobalLexical, &[0, 2]).unwrap();
    words.extend(encode_instruction(Opcode::LoadUndefined, &[0]).unwrap());
    words.extend(encode_instruction(Opcode::Return, &[0]).unwrap());
    assert!(matches!(
        Bytecode::from_words(words).verify(context()),
        Err(VerifyError::InvalidBooleanOperand { operand: 2, .. })
    ));
}

#[test]
fn load_undefined_uses_one_register_operand() {
    let words = encode_instruction(Opcode::LoadUndefined, &[7]).unwrap();
    let decoded = decode_instruction(&words, WordOffset::new(0)).unwrap();
    assert_eq!(decoded.opcode, Opcode::LoadUndefined);
    assert_eq!(decoded.operand_count, 1);
    assert_eq!(decoded.operands[0], 7);
    assert_eq!(MAX_ENCODED_INSTRUCTION_WORDS, 4);
}

#[test]
fn return_undefined_verifies_without_a_register_file() {
    let words = encode_instruction(Opcode::ReturnUndefined, &[]).unwrap();
    let decoded = decode_instruction(&words, WordOffset::new(0)).unwrap();
    assert_eq!(decoded.opcode, Opcode::ReturnUndefined);
    assert_eq!(decoded.operand_count, 0);
    assert!(decoded.opcode.is_terminal());
    let mut context = context();
    context.register_count = 0;
    assert!(Bytecode::from_words(words).verify(context).is_ok());
}

#[test]
fn non_numeric_immediate_loads_use_one_register_operand() {
    for opcode in [Opcode::LoadNull, Opcode::LoadFalse, Opcode::LoadTrue] {
        let words = encode_instruction(opcode, &[7]).unwrap();
        let decoded = decode_instruction(&words, WordOffset::new(0)).unwrap();
        assert_eq!(decoded.opcode, opcode);
        assert_eq!(decoded.operand_count, 1);
        assert_eq!(decoded.operands[0], 7);
    }
}

#[test]
fn builder_patches_forward_jump_and_freezes_metadata() {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(4, 1);
    let end = builder.new_label().unwrap();
    builder.emit_jump(end, span).unwrap();
    builder.emit(Opcode::Nop, &[], span).unwrap();
    builder.bind_label(end).unwrap();
    builder.emit(Opcode::Return, &[0], span).unwrap();

    let (bytecode, source_map, registers) = builder.finish().unwrap();
    assert_eq!(registers, 1);
    assert_eq!(source_map.len(), 3);
    assert_eq!(
        decode_instruction(bytecode.words(), WordOffset::new(0))
            .unwrap()
            .operand(0),
        Some(3)
    );
    assert!(bytecode.verify(context()).is_ok());
}

#[test]
/// Exercises the shared patch format for each conditional branch semantic.
fn builder_patches_every_conditional_branch_kind() {
    for opcode in [
        Opcode::JumpIfFalse,
        Opcode::JumpIfTrue,
        Opcode::JumpIfNotNullish,
    ] {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(4, 1);
        let end = builder.new_label().unwrap();
        match opcode {
            Opcode::JumpIfFalse => builder.emit_jump_if_false(RegisterId::new(0), end, span),
            Opcode::JumpIfTrue => builder.emit_jump_if_true(RegisterId::new(0), end, span),
            Opcode::JumpIfNotNullish => {
                builder.emit_jump_if_not_nullish(RegisterId::new(0), end, span)
            }
            _ => unreachable!(),
        }
        .unwrap();
        builder.bind_label(end).unwrap();
        builder.emit(Opcode::Return, &[0], span).unwrap();
        let (bytecode, _, registers) = builder.finish().unwrap();
        let branch = decode_instruction(bytecode.words(), WordOffset::new(0)).unwrap();
        assert_eq!(branch.opcode, opcode);
        assert_eq!(branch.operand(1), Some(3));
        assert_eq!(registers, 1);
        assert!(bytecode.verify(context()).is_ok());
    }
}

#[test]
fn builder_rejects_unbound_labels() {
    let span = SourceSpan { start: 0, end: 0 };
    let mut builder = BytecodeBuilder::default();
    let label = builder.new_label().unwrap();
    builder.emit_jump(label, span).unwrap();
    assert!(matches!(
        builder.finish(),
        Err(BuilderError::UnboundLabel(found)) if found == label
    ));
}

#[test]
fn builder_tracks_call_register_window() {
    let span = SourceSpan { start: 0, end: 0 };
    let mut builder = BytecodeBuilder::default();
    builder.emit(Opcode::Call, &[0, 1, 2], span).unwrap();
    builder.emit(Opcode::Return, &[0], span).unwrap();
    let (_, _, register_count) = builder.finish().unwrap();
    assert_eq!(register_count, 4);
}

#[test]
fn compiled_module_freezes_async_metadata_without_runtime_values() {
    let mut words = encode_instruction(Opcode::Await, &[0, 0, 0]).unwrap();
    words.extend(encode_instruction(Opcode::Return, &[0]).unwrap());
    let mut metadata = FunctionMetadata::new(
        FunctionKind::Async,
        FunctionLayout {
            register_count: 1,
            feedback_slot_count: 3,
            environment_slot_count: 1,
            max_handler_depth: 1,
            max_completion_depth: 2,
            ..FunctionLayout::default()
        },
    );
    metadata.source_map = vec![
        SourceMapEntry {
            offset: WordOffset::new(0),
            span: SourceSpan { start: 0, end: 1 },
        },
        SourceMapEntry {
            offset: WordOffset::new(1),
            span: SourceSpan { start: 0, end: 1 },
        },
    ]
    .into();
    metadata.handlers = vec![HandlerEntry {
        protected_start: WordOffset::new(0),
        protected_end: WordOffset::new(1),
        handler: WordOffset::new(1),
        handler_end: WordOffset::new(1),
        kind: HandlerKind::Catch,
        environment_depth: 0,
    }]
    .into();
    metadata.suspend_points = vec![SuspendPoint {
        id: SuspendPointId::new(0),
        instruction: WordOffset::new(0),
        resume_offset: WordOffset::new(1),
        destination: RegisterId::new(0),
        completion_depth: 1,
    }]
    .into();
    metadata.feedback_sites = vec![FeedbackSite {
        offset: WordOffset::new(0),
        slot: FeedbackSlot::new(2),
    }]
    .into();
    metadata.binding_plan = vec![BindingPlanEntry {
        name: Arc::from("value"),
        location: BindingLocation::FrameRegister(RegisterId::new(0)),
        mutable: true,
    }]
    .into();
    metadata.environment_record_kind = EnvironmentRecordKind::Function;
    metadata.environment_slots = vec![EnvironmentSlotMetadata {
        name: Arc::from("captured"),
        mutable: false,
        initialized: false,
    }]
    .into();

    let module = CompiledModule::new(
        Arc::from("x"),
        vec![
            BytecodeConstant::NumberBits(1.0_f64.to_bits()),
            BytecodeConstant::string_from_utf16(vec![0xd800]),
        ],
        Vec::new(),
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            Bytecode::from_words(words),
            metadata,
        )],
        FunctionId::new(0),
    )
    .unwrap();

    assert_eq!(module.source(), "x");
    assert_eq!(module.entry_function(), FunctionId::new(0));
    let function = module.function(FunctionId::new(0)).unwrap();
    assert_eq!(function.suspend_points().len(), 1);
    assert_eq!(
        function.binding_plan(),
        &[BindingPlanEntry {
            name: Arc::from("value"),
            location: BindingLocation::FrameRegister(RegisterId::new(0)),
            mutable: true,
        }]
    );
    assert_eq!(
        function.environment_slots(),
        &[EnvironmentSlotMetadata {
            name: Arc::from("captured"),
            mutable: false,
            initialized: false,
        }]
    );
    assert_eq!(
        disassemble(function).unwrap(),
        "000000 [0..1] Await r0, r0, suspend=0 feedback=2\n000001 [0..1] Return r0\n"
    );
    assert!(matches!(
        &module.constants()[1],
        BytecodeConstant::String(value) if value.as_ref() == [0xd800]
    ));
}

#[test]
fn compiled_module_rejects_sloppy_module_metadata() {
    let mut metadata = FunctionMetadata::new(
        FunctionKind::Module,
        FunctionLayout {
            register_count: 1,
            ..FunctionLayout::default()
        },
    );
    metadata.strictness = FunctionStrictness::Sloppy;
    let template = CompiledFunctionTemplate::new(
        FunctionId::new(0),
        Bytecode::from_words(encode_instruction(Opcode::Return, &[0]).unwrap()),
        metadata,
    );
    assert!(matches!(
        CompiledModule::new(Arc::from(""), Vec::new(), Vec::new(), vec![template], FunctionId::new(0)),
        Err(ModuleBuildError::InvalidFunctionStrictness {
            function,
            kind: FunctionKind::Module,
            strictness: FunctionStrictness::Sloppy,
        }) if function == FunctionId::new(0)
    ));
}

/// Builds one terminal function around caller-selected binding metadata for verifier tests.
fn binding_plan_module(
    binding: BindingPlanEntry,
    layout: FunctionLayout,
    scope_names: Vec<Arc<str>>,
) -> Result<CompiledModule, ModuleBuildError> {
    let words = encode_instruction(Opcode::Return, &[0]).unwrap();
    let mut metadata = FunctionMetadata::new(FunctionKind::Script, layout);
    metadata.binding_plan = vec![binding].into();
    CompiledModule::new(
        Arc::from("x"),
        Vec::new(),
        scope_names,
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            Bytecode::from_words(words),
            metadata,
        )],
        FunctionId::new(0),
    )
}

/// Builds one terminal function around a complete caller-selected binding plan.
fn binding_plan_entries_module(
    kind: FunctionKind,
    bindings: Vec<BindingPlanEntry>,
    layout: FunctionLayout,
    record_kind: EnvironmentRecordKind,
    environment_slots: Vec<EnvironmentSlotMetadata>,
) -> Result<CompiledModule, ModuleBuildError> {
    let words = encode_instruction(Opcode::Return, &[0]).unwrap();
    let mut metadata = FunctionMetadata::new(kind, layout);
    metadata.binding_plan = bindings.into();
    metadata.environment_record_kind = record_kind;
    metadata.environment_slots = environment_slots.into();
    CompiledModule::new(
        Arc::from("x"),
        Vec::new(),
        Vec::new(),
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            Bytecode::from_words(words),
            metadata,
        )],
        FunctionId::new(0),
    )
}

fn environment_slot(
    name: &'static str,
    mutable: bool,
    initialized: bool,
) -> EnvironmentSlotMetadata {
    EnvironmentSlotMetadata {
        name: Arc::from(name),
        mutable,
        initialized,
    }
}

#[test]
fn environment_record_kind_defaults_cover_every_function_kind() {
    for (kind, expected) in [
        (FunctionKind::Script, EnvironmentRecordKind::Global),
        (FunctionKind::Module, EnvironmentRecordKind::Module),
        (FunctionKind::Ordinary, EnvironmentRecordKind::Function),
        (FunctionKind::Generator, EnvironmentRecordKind::Function),
        (FunctionKind::Async, EnvironmentRecordKind::Function),
        (
            FunctionKind::AsyncGenerator,
            EnvironmentRecordKind::Function,
        ),
    ] {
        assert_eq!(EnvironmentRecordKind::for_function_kind(kind), expected);
    }
}

#[test]
/// Proves slot plans are exact, dense by slice index, and tied to owner binding identity.
fn compiled_module_verifies_environment_slot_metadata() {
    let layout = FunctionLayout {
        register_count: 1,
        environment_slot_count: 2,
        ..FunctionLayout::default()
    };
    let valid = vec![
        BindingPlanEntry {
            name: Arc::from("mutable"),
            location: BindingLocation::Environment { depth: 0, slot: 0 },
            mutable: true,
        },
        BindingPlanEntry {
            name: Arc::from("fixed"),
            location: BindingLocation::Environment { depth: 0, slot: 1 },
            mutable: false,
        },
    ];
    let module = binding_plan_entries_module(
        FunctionKind::Ordinary,
        valid,
        layout,
        EnvironmentRecordKind::Declarative,
        vec![
            environment_slot("mutable", true, false),
            environment_slot("fixed", false, false),
        ],
    )
    .unwrap();
    let function = module.function(FunctionId::new(0)).unwrap();
    assert_eq!(
        function.environment_record_kind(),
        EnvironmentRecordKind::Declarative
    );
    assert_eq!(function.environment_slots()[1].name.as_ref(), "fixed");
    assert!(
        binding_plan_entries_module(
            FunctionKind::Module,
            Vec::new(),
            FunctionLayout {
                register_count: 1,
                environment_slot_count: 1,
                ..FunctionLayout::default()
            },
            EnvironmentRecordKind::Module,
            vec![environment_slot("module", false, false)],
        )
        .is_ok()
    );

    assert!(matches!(
        binding_plan_entries_module(
            FunctionKind::Ordinary,
            Vec::new(),
            layout,
            EnvironmentRecordKind::Function,
            Vec::new(),
        ),
        Err(ModuleBuildError::EnvironmentSlotMetadataCountMismatch {
            expected: 2,
            actual: 0,
            ..
        })
    ));
    assert!(matches!(
        binding_plan_entries_module(
            FunctionKind::Ordinary,
            vec![BindingPlanEntry {
                name: Arc::from("a"),
                location: BindingLocation::Environment { depth: 0, slot: 2 },
                mutable: true,
            }],
            layout,
            EnvironmentRecordKind::Function,
            vec![
                environment_slot("a", true, true),
                environment_slot("b", true, true),
            ],
        ),
        Err(ModuleBuildError::BindingEnvironmentSlotOutOfRange { .. })
    ));
    assert!(matches!(
        binding_plan_entries_module(
            FunctionKind::Ordinary,
            vec![BindingPlanEntry {
                name: Arc::from("value"),
                location: BindingLocation::Environment { depth: 0, slot: 0 },
                mutable: false,
            }],
            FunctionLayout {
                register_count: 1,
                environment_slot_count: 1,
                ..FunctionLayout::default()
            },
            EnvironmentRecordKind::Function,
            vec![environment_slot("value", true, true)],
        ),
        Err(ModuleBuildError::EnvironmentSlotBindingMismatch { .. })
    ));
    assert!(matches!(
        binding_plan_entries_module(
            FunctionKind::Ordinary,
            Vec::new(),
            FunctionLayout {
                register_count: 1,
                environment_slot_count: 1,
                ..FunctionLayout::default()
            },
            EnvironmentRecordKind::Declarative,
            vec![environment_slot("", true, false)],
        ),
        Err(ModuleBuildError::EmptyEnvironmentSlotName { slot: 0, .. })
    ));
}

#[test]
/// Rejects every bounded binding location before malformed immutable code reaches the VM.
fn compiled_module_rejects_invalid_binding_plans() {
    let frame = BindingPlanEntry {
        name: Arc::from("value"),
        location: BindingLocation::FrameRegister(RegisterId::new(1)),
        mutable: true,
    };
    assert!(matches!(
        binding_plan_module(
            frame,
            FunctionLayout {
                register_count: 1,
                ..FunctionLayout::default()
            },
            vec![Arc::from("value")],
        ),
        Err(ModuleBuildError::BindingRegisterOutOfRange { .. })
    ));

    let environment = BindingPlanEntry {
        name: Arc::from("value"),
        location: BindingLocation::Environment { depth: 0, slot: 1 },
        mutable: true,
    };
    assert!(matches!(
        binding_plan_module(
            environment,
            FunctionLayout {
                register_count: 1,
                environment_slot_count: 1,
                ..FunctionLayout::default()
            },
            vec![Arc::from("value")],
        ),
        Err(ModuleBuildError::BindingEnvironmentSlotOutOfRange { .. })
    ));

    let name = BindingPlanEntry {
        name: Arc::from(""),
        location: BindingLocation::GlobalProperty,
        mutable: true,
    };
    assert!(matches!(
        binding_plan_module(
            name,
            FunctionLayout {
                register_count: 1,
                ..FunctionLayout::default()
            },
            vec![Arc::from("value")],
        ),
        Err(ModuleBuildError::EmptyBindingName { .. })
    ));
}

#[test]
fn compiled_module_rejects_invalid_pool_and_suspend_references() {
    let mut out_of_range_constant = encode_instruction(Opcode::LoadConstant, &[0, 1]).unwrap();
    out_of_range_constant.extend(encode_instruction(Opcode::Return, &[0]).unwrap());
    let metadata = FunctionMetadata::new(
        FunctionKind::Script,
        FunctionLayout {
            register_count: 1,
            ..FunctionLayout::default()
        },
    );
    let error = CompiledModule::new(
        Arc::from("x"),
        vec![BytecodeConstant::NumberBits(0)],
        Vec::new(),
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            Bytecode::from_words(out_of_range_constant),
            metadata,
        )],
        FunctionId::new(0),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ModuleBuildError::VerifyFunction {
            error: VerifyError::ConstantOutOfRange { .. },
            ..
        }
    ));

    let mut await_without_metadata = encode_instruction(Opcode::Await, &[0, 0, 0]).unwrap();
    await_without_metadata.extend(encode_instruction(Opcode::Return, &[0]).unwrap());
    let error = CompiledModule::new(
        Arc::from("x"),
        Vec::new(),
        Vec::new(),
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            Bytecode::from_words(await_without_metadata),
            FunctionMetadata::new(
                FunctionKind::Async,
                FunctionLayout {
                    register_count: 1,
                    ..FunctionLayout::default()
                },
            ),
        )],
        FunctionId::new(0),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ModuleBuildError::SuspendPointMissing { .. }
    ));
}

#[test]
fn compiled_module_rejects_crossing_and_underreserved_handler_tables() {
    let mut words = encode_instruction(Opcode::Nop, &[]).unwrap();
    words.extend(encode_instruction(Opcode::Nop, &[]).unwrap());
    words.extend(encode_instruction(Opcode::Nop, &[]).unwrap());
    words.extend(encode_instruction(Opcode::Return, &[0]).unwrap());
    let crossing_handlers: Arc<[HandlerEntry]> = vec![
        HandlerEntry {
            protected_start: WordOffset::new(0),
            protected_end: WordOffset::new(2),
            handler: WordOffset::new(3),
            handler_end: WordOffset::new(3),
            kind: HandlerKind::Catch,
            environment_depth: 0,
        },
        HandlerEntry {
            protected_start: WordOffset::new(1),
            protected_end: WordOffset::new(3),
            handler: WordOffset::new(3),
            handler_end: WordOffset::new(3),
            kind: HandlerKind::Catch,
            environment_depth: 0,
        },
    ]
    .into();
    let mut metadata = FunctionMetadata::new(
        FunctionKind::Script,
        FunctionLayout {
            register_count: 1,
            max_handler_depth: 2,
            ..FunctionLayout::default()
        },
    );
    metadata.handlers = crossing_handlers;
    let error = CompiledModule::new(
        Arc::from(""),
        Vec::new(),
        Vec::new(),
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            Bytecode::from_words(words.clone()),
            metadata,
        )],
        FunctionId::new(0),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ModuleBuildError::HandlerTableNotProperlyNested { .. }
    ));

    let mut metadata = FunctionMetadata::new(
        FunctionKind::Script,
        FunctionLayout {
            register_count: 1,
            ..FunctionLayout::default()
        },
    );
    metadata.handlers = vec![HandlerEntry {
        protected_start: WordOffset::new(0),
        protected_end: WordOffset::new(1),
        handler: WordOffset::new(3),
        handler_end: WordOffset::new(3),
        kind: HandlerKind::Catch,
        environment_depth: 0,
    }]
    .into();
    let error = CompiledModule::new(
        Arc::from(""),
        Vec::new(),
        Vec::new(),
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            Bytecode::from_words(words),
            metadata,
        )],
        FunctionId::new(0),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ModuleBuildError::InvalidFunctionLayout { .. }
    ));
}

#[test]
/// Accepts the minimal verified finalizer and rejects completion opcodes outside its ranges.
fn compiled_module_cross_validates_finally_control_opcodes() {
    let mut valid_words = encode_instruction(Opcode::Nop, &[]).unwrap();
    valid_words.extend(encode_instruction(Opcode::EnterFinally, &[]).unwrap());
    valid_words.extend(encode_instruction(Opcode::ResumeCompletion, &[]).unwrap());
    valid_words.extend(encode_instruction(Opcode::ReturnUndefined, &[]).unwrap());
    let handler = HandlerEntry {
        protected_start: WordOffset::new(0),
        protected_end: WordOffset::new(2),
        handler: WordOffset::new(2),
        handler_end: WordOffset::new(3),
        kind: HandlerKind::Finally,
        environment_depth: 0,
    };
    let metadata = |handler| {
        let mut metadata = FunctionMetadata::new(
            FunctionKind::Script,
            FunctionLayout {
                max_handler_depth: 1,
                max_completion_depth: 1,
                ..FunctionLayout::default()
            },
        );
        metadata.handlers = vec![handler].into();
        metadata
    };
    CompiledModule::new(
        Arc::from(""),
        Vec::new(),
        Vec::new(),
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            Bytecode::from_words(valid_words),
            metadata(handler),
        )],
        FunctionId::new(0),
    )
    .unwrap();

    let mut outside_words = encode_instruction(Opcode::Nop, &[]).unwrap();
    outside_words.extend(encode_instruction(Opcode::EnterFinally, &[]).unwrap());
    outside_words.extend(encode_instruction(Opcode::ResumeCompletion, &[]).unwrap());
    outside_words.extend(encode_instruction(Opcode::ReturnUndefined, &[]).unwrap());
    let outside_handler = HandlerEntry {
        protected_end: WordOffset::new(1),
        ..handler
    };
    let error = CompiledModule::new(
        Arc::from(""),
        Vec::new(),
        Vec::new(),
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            Bytecode::from_words(outside_words),
            metadata(outside_handler),
        )],
        FunctionId::new(0),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ModuleBuildError::InvalidFinallyInstruction {
            opcode: Opcode::EnterFinally,
            ..
        }
    ));
}

#[test]
fn compiled_module_rejects_finalizer_without_terminal_resume() {
    let mut words = encode_instruction(Opcode::Nop, &[]).unwrap();
    words.extend(encode_instruction(Opcode::EnterFinally, &[]).unwrap());
    words.extend(encode_instruction(Opcode::Nop, &[]).unwrap());
    words.extend(encode_instruction(Opcode::ReturnUndefined, &[]).unwrap());
    let mut metadata = FunctionMetadata::new(
        FunctionKind::Script,
        FunctionLayout {
            max_handler_depth: 1,
            max_completion_depth: 1,
            ..FunctionLayout::default()
        },
    );
    metadata.handlers = vec![HandlerEntry {
        protected_start: WordOffset::new(0),
        protected_end: WordOffset::new(2),
        handler: WordOffset::new(2),
        handler_end: WordOffset::new(3),
        kind: HandlerKind::Finally,
        environment_depth: 0,
    }]
    .into();
    let error = CompiledModule::new(
        Arc::from(""),
        Vec::new(),
        Vec::new(),
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            Bytecode::from_words(words),
            metadata,
        )],
        FunctionId::new(0),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ModuleBuildError::InvalidHandlerRange { .. }
    ));
}

#[test]
/// Rejects abrupt-finally opcodes that neither cross nor originate in a finalizer boundary.
fn compiled_module_rejects_spurious_abrupt_finally_targets() {
    let mut uncovered = encode_instruction(Opcode::BreakThroughFinally, &[2]).unwrap();
    uncovered.extend(encode_instruction(Opcode::ReturnUndefined, &[]).unwrap());
    let error = CompiledModule::new(
        Arc::from(""),
        Vec::new(),
        Vec::new(),
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            Bytecode::from_words(uncovered),
            FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default()),
        )],
        FunctionId::new(0),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ModuleBuildError::InvalidFinallyInstruction {
            opcode: Opcode::BreakThroughFinally,
            ..
        }
    ));

    let mut retained = encode_instruction(Opcode::Nop, &[]).unwrap();
    retained.extend(encode_instruction(Opcode::ContinueThroughFinally, &[0]).unwrap());
    retained.extend(encode_instruction(Opcode::ResumeCompletion, &[]).unwrap());
    retained.extend(encode_instruction(Opcode::ReturnUndefined, &[]).unwrap());
    let mut metadata = FunctionMetadata::new(
        FunctionKind::Script,
        FunctionLayout {
            max_handler_depth: 1,
            max_completion_depth: 1,
            ..FunctionLayout::default()
        },
    );
    metadata.handlers = vec![HandlerEntry {
        protected_start: WordOffset::new(0),
        protected_end: WordOffset::new(3),
        handler: WordOffset::new(3),
        handler_end: WordOffset::new(4),
        kind: HandlerKind::Finally,
        environment_depth: 0,
    }]
    .into();
    let error = CompiledModule::new(
        Arc::from(""),
        Vec::new(),
        Vec::new(),
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            Bytecode::from_words(retained),
            metadata,
        )],
        FunctionId::new(0),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ModuleBuildError::InvalidFinallyInstruction {
            opcode: Opcode::ContinueThroughFinally,
            ..
        }
    ));
}

#[test]
/// Counts an inner catch while its containing finalizer completion remains active.
fn compiled_module_counts_handlers_nested_in_finalizer_execution() {
    let opcodes = [
        Opcode::Nop,
        Opcode::Nop,
        Opcode::Nop,
        Opcode::Nop,
        Opcode::Nop,
        Opcode::ResumeCompletion,
        Opcode::ReturnUndefined,
    ];
    let words = opcodes
        .into_iter()
        .flat_map(|opcode| encode_instruction(opcode, &[]).unwrap())
        .collect::<Vec<_>>();
    let handlers: Arc<[HandlerEntry]> = vec![
        HandlerEntry {
            protected_start: WordOffset::new(0),
            protected_end: WordOffset::new(1),
            handler: WordOffset::new(1),
            handler_end: WordOffset::new(6),
            kind: HandlerKind::Finally,
            environment_depth: 0,
        },
        HandlerEntry {
            protected_start: WordOffset::new(2),
            protected_end: WordOffset::new(3),
            handler: WordOffset::new(3),
            handler_end: WordOffset::new(3),
            kind: HandlerKind::Catch,
            environment_depth: 0,
        },
    ]
    .into();
    let mut metadata = FunctionMetadata::new(
        FunctionKind::Script,
        FunctionLayout {
            max_handler_depth: 1,
            max_completion_depth: 1,
            ..FunctionLayout::default()
        },
    );
    metadata.handlers = handlers;
    let error = CompiledModule::new(
        Arc::from(""),
        Vec::new(),
        Vec::new(),
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            Bytecode::from_words(words),
            metadata,
        )],
        FunctionId::new(0),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ModuleBuildError::InvalidFunctionLayout { .. }
    ));
}

#[test]
fn compiled_module_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CompiledModule>();
}

#[test]
fn decoder_preserves_maximum_logical_operand() {
    let words = encode_instruction(Opcode::Jump, &[u32::MAX]).unwrap();
    assert_eq!(
        decode_instruction(&words, WordOffset::new(0))
            .unwrap()
            .operand(0),
        Some(u32::MAX)
    );
}

proptest! {
    #[test]
    fn add_roundtrips_all_encoding_widths(
        compact in 0_u32..=u8::MAX as u32,
        normal in (u8::MAX as u32 + 1)..=u16::MAX as u32,
        wide in (u16::MAX as u32 + 1)..=u32::MAX,
    ) {
        for operands in [[compact, 1, 2], [normal, 1, 2], [wide, 1, 2]] {
            let words = encode_instruction(Opcode::Add, &operands).unwrap();
            let decoded = decode_instruction(&words, WordOffset::new(0)).unwrap();
            prop_assert_eq!(decoded.operands, operands);
            prop_assert_eq!(decoded.operand_count, 3);
        }
    }

    #[test]
    fn arbitrary_word_streams_never_panic(
        words in proptest::collection::vec(any::<u32>(), 0..64),
        offset in 0_u32..70,
    ) {
        let _ = decode_instruction(&words, WordOffset::new(offset));
        let _ = Bytecode::from_words(words).verify(context());
    }
}
