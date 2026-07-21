//! Bytecode and compiled-module structural verification.

use super::{
    BindingLocation, BindingPlanEntry, Bytecode, DecodeError, DecodedInstruction,
    EnvironmentSlotMetadata, FeedbackSite, FunctionId, FunctionKind, FunctionLayout, HandlerEntry,
    HandlerKind, ModuleBuildError, Opcode, SourceMapEntry, SuspendPoint, SuspendPointId,
    VerifiedBytecode, WordOffset, decode_instruction,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifyContext {
    pub register_count: u32,
    pub constant_count: u32,
    pub function_count: u32,
    pub scope_name_count: u32,
    pub max_environment_slot_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerifyError {
    Decode(DecodeError),
    CodeTooLarge {
        word_count: usize,
    },
    RegisterOutOfRange {
        offset: WordOffset,
        register: u32,
        register_count: u32,
    },
    ConstantOutOfRange {
        offset: WordOffset,
        constant: u32,
        constant_count: u32,
    },
    ScopeNameOutOfRange {
        offset: WordOffset,
        scope_name: u32,
        scope_name_count: u32,
    },
    FunctionOutOfRange {
        offset: WordOffset,
        function: u32,
        function_count: u32,
    },
    EnvironmentSlotOutOfRange {
        offset: WordOffset,
        slot: u32,
        max_environment_slot_count: u32,
    },
    InvalidBooleanOperand {
        offset: WordOffset,
        operand: u32,
    },
    InvalidCallArgumentWindow {
        offset: WordOffset,
        callee: u32,
        argument_count: u32,
        register_count: u32,
    },
    InvalidJumpTarget {
        offset: WordOffset,
        target: u32,
    },
    MissingTerminalInstruction,
}

/// Layout metadata must permit the VM to reserve all function-local windows before dispatch starts.
pub(super) fn validate_function_layout(
    function: FunctionId,
    layout: FunctionLayout,
    handler_depth: HandlerDepth,
    scope_name_count: u32,
) -> Result<(), ModuleBuildError> {
    if layout.argument_count > layout.register_count
        || layout.temporary_register_count > layout.register_count - layout.argument_count
        || layout.max_handler_depth < handler_depth.handlers
        || layout.max_completion_depth < handler_depth.finally_handlers
        || layout
            .name_scope
            .is_some_and(|name_scope| name_scope >= scope_name_count)
    {
        return Err(ModuleBuildError::InvalidFunctionLayout { function, layout });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct HandlerDepth {
    handlers: u32,
    finally_handlers: u32,
}

/// Source maps may omit instructions but each retained entry must be an ordered valid instruction start.
pub(super) fn validate_source_map(
    function: FunctionId,
    source_map: &[SourceMapEntry],
    bytecode: &VerifiedBytecode,
    source_len: u32,
) -> Result<(), ModuleBuildError> {
    let mut previous: Option<WordOffset> = None;
    for entry in source_map {
        if !bytecode.is_instruction_start(entry.offset) {
            return Err(ModuleBuildError::SourceMapOffsetNotInstructionStart {
                function,
                offset: entry.offset,
            });
        }
        if entry.span.start > entry.span.end || entry.span.end > source_len {
            return Err(ModuleBuildError::SourceMapOutOfBounds {
                function,
                span: entry.span,
                source_len,
            });
        }
        if let Some(previous) = previous.filter(|previous| entry.offset.index() <= previous.index())
        {
            return Err(ModuleBuildError::SourceMapNotMonotonic {
                function,
                previous,
                current: entry.offset,
            });
        }
        previous = Some(entry.offset);
    }
    Ok(())
}

/// Handler ranges are half-open and must refer only to decoded instruction boundaries in their function.
pub(super) fn validate_handlers(
    function: FunctionId,
    handlers: &[HandlerEntry],
    bytecode: &VerifiedBytecode,
) -> Result<HandlerDepth, ModuleBuildError> {
    let code_len = bytecode.bytecode().words().len() as u32;
    for &handler in handlers {
        let start_is_valid = bytecode.is_instruction_start(handler.protected_start);
        let end_is_valid = handler.protected_end.index() == code_len
            || bytecode.is_instruction_start(handler.protected_end);
        let handler_end_is_valid = handler.handler_end.index() == code_len
            || bytecode.is_instruction_start(handler.handler_end);
        let handler_kind_is_valid = match handler.kind {
            HandlerKind::Catch => handler.handler_end == handler.handler,
            HandlerKind::Finally | HandlerKind::IteratorClose => {
                handler.handler.index() < handler.handler_end.index()
                    && finalizer_ends_with_resume(bytecode, handler.handler_end)
            }
        };
        if !start_is_valid
            || !end_is_valid
            || !bytecode.is_instruction_start(handler.handler)
            || !handler_end_is_valid
            || !handler_kind_is_valid
            || handler.protected_start.index() >= handler.protected_end.index()
        {
            return Err(ModuleBuildError::InvalidHandlerRange { function, handler });
        }
        if handler.protected_start.index() <= handler.handler.index()
            && handler.handler.index() < handler.protected_end.index()
        {
            return Err(ModuleBuildError::HandlerTargetInsideProtectedRange { function, handler });
        }
    }
    let mut max_depth = HandlerDepth::default();
    for (index, &current) in handlers.iter().enumerate() {
        for &previous in &handlers[..index] {
            if previous.protected_start.index() > current.protected_start.index()
                || (previous.protected_start == current.protected_start
                    && previous.protected_end.index() <= current.protected_end.index())
                || crosses_handler_ranges(previous, current)
            {
                return Err(ModuleBuildError::HandlerTableNotProperlyNested {
                    function,
                    previous,
                    current,
                });
            }
            if crosses_finalizer_ranges(previous, current) {
                return Err(ModuleBuildError::HandlerTableNotProperlyNested {
                    function,
                    previous,
                    current,
                });
            }
        }
        let mut depth = 0u32;
        for &candidate in &handlers[..=index] {
            if handler_contains(candidate, current) {
                depth += 1;
            }
        }
        max_depth.handlers = max_depth.handlers.max(depth);
    }
    for &current in handlers
        .iter()
        .filter(|handler| handler.kind.is_finalizer())
    {
        let active_depth = handlers
            .iter()
            .filter(|candidate| finalizer_contains(**candidate, current))
            .count() as u32;
        max_depth.handlers = max_depth.handlers.max(active_depth);
        max_depth.finally_handlers = max_depth.finally_handlers.max(active_depth);
    }
    for &current in handlers {
        let protected_depth = handlers
            .iter()
            .filter(|candidate| handler_contains(**candidate, current))
            .count() as u32;
        let active_outer_finalizers = handlers
            .iter()
            .filter(|candidate| finalizer_executes_around(**candidate, current))
            .count() as u32;
        max_depth.handlers = max_depth
            .handlers
            .max(protected_depth.saturating_add(active_outer_finalizers));
    }
    Ok(max_depth)
}

/// Confirms a finalizer's exclusive end is the fallthrough after `ResumeCompletion`.
fn finalizer_ends_with_resume(bytecode: &VerifiedBytecode, end: WordOffset) -> bool {
    let words = bytecode.bytecode().words();
    let mut offset = 0u32;
    while offset < end.index() {
        let Ok(instruction) = super::decode_instruction(words, WordOffset::new(offset)) else {
            return false;
        };
        let next = offset + u32::from(instruction.word_len);
        if next == end.index() {
            return instruction.opcode == Opcode::ResumeCompletion;
        }
        offset = next;
    }
    false
}

fn handler_contains(outer: HandlerEntry, inner: HandlerEntry) -> bool {
    outer.protected_start.index() <= inner.protected_start.index()
        && inner.protected_end.index() <= outer.protected_end.index()
}

fn crosses_handler_ranges(left: HandlerEntry, right: HandlerEntry) -> bool {
    left.protected_start.index() < right.protected_start.index()
        && right.protected_start.index() < left.protected_end.index()
        && left.protected_end.index() < right.protected_end.index()
}

fn finalizer_contains(outer: HandlerEntry, inner: HandlerEntry) -> bool {
    outer.kind.is_finalizer()
        && outer.handler.index() <= inner.handler.index()
        && inner.handler_end.index() <= outer.handler_end.index()
}

fn finalizer_executes_around(outer: HandlerEntry, inner: HandlerEntry) -> bool {
    outer.kind.is_finalizer()
        && outer.handler.index() <= inner.protected_start.index()
        && inner.protected_end.index() <= outer.handler_end.index()
}

fn crosses_finalizer_ranges(left: HandlerEntry, right: HandlerEntry) -> bool {
    if !left.kind.is_finalizer() || !right.kind.is_finalizer() {
        return false;
    }
    let (first, second) = if left.handler.index() <= right.handler.index() {
        (left, right)
    } else {
        (right, left)
    };
    first.handler.index() < second.handler.index()
        && second.handler.index() < first.handler_end.index()
        && first.handler_end.index() < second.handler_end.index()
}

/// Cross-validates completion opcodes against immutable finalizer metadata after decoding.
pub(super) fn validate_finally_instructions(
    function: FunctionId,
    handlers: &[HandlerEntry],
    bytecode: &VerifiedBytecode,
) -> Result<(), ModuleBuildError> {
    let words = bytecode.bytecode().words();
    let mut offset = 0u32;
    while (offset as usize) < words.len() {
        let instruction = super::decode_instruction(words, WordOffset::new(offset))
            .expect("verified bytecode remains decodable");
        let next = offset + u32::from(instruction.word_len);
        let valid = match instruction.opcode {
            Opcode::EnterFinally => handlers.iter().any(|handler| {
                handler.kind.is_finalizer()
                    && handler.protected_start.index() <= offset
                    && offset < handler.protected_end.index()
            }),
            Opcode::ResumeCompletion => handlers.iter().any(|handler| {
                handler.kind.is_finalizer()
                    && handler.handler.index() <= offset
                    && handler.handler_end.index() == next
            }),
            Opcode::BreakThroughFinally | Opcode::ContinueThroughFinally => {
                let target = instruction.operands[0];
                handlers.iter().any(|handler| {
                    handler.kind.is_finalizer()
                        && ((handler.protected_start.index() <= offset
                            && offset < handler.protected_end.index()
                            && !(handler.protected_start.index() <= target
                                && target < handler.protected_end.index()))
                            || (handler.handler.index() <= offset
                                && offset < handler.handler_end.index()
                                && !(handler.handler.index() <= target
                                    && target < handler.handler_end.index())))
                })
            }
            _ => true,
        };
        if !valid {
            return Err(ModuleBuildError::InvalidFinallyInstruction {
                function,
                offset: WordOffset::new(offset),
                opcode: instruction.opcode,
            });
        }
        offset = next;
    }
    Ok(())
}

/// Restricts derived-constructor opcodes and class template references after stream verification.
pub(super) fn validate_class_instructions(
    function: FunctionId,
    kind: FunctionKind,
    bytecode: &VerifiedBytecode,
    function_kinds: &[FunctionKind],
) -> Result<(), ModuleBuildError> {
    let words = bytecode.bytecode().words();
    let mut offset = 0u32;
    while (offset as usize) < words.len() {
        let word_offset = WordOffset::new(offset);
        let instruction = decode_instruction(words, word_offset).map_err(|_| {
            ModuleBuildError::VerifiedBytecodeDecodeInvariant {
                function,
                offset: word_offset,
            }
        })?;
        if matches!(
            instruction.opcode,
            Opcode::SuperConstruct | Opcode::InitializeThis
        ) && kind != FunctionKind::DerivedClassConstructor
        {
            return Err(ModuleBuildError::InvalidClassInstruction {
                function,
                kind,
                offset: word_offset,
                opcode: instruction.opcode,
            });
        }
        if instruction.opcode == Opcode::CreateClass {
            let target = FunctionId::new(instruction.operands[1]);
            let target_kind = function_kinds[target.index() as usize];
            if target_kind != FunctionKind::DerivedClassConstructor {
                return Err(ModuleBuildError::InvalidClassConstructorTarget {
                    function,
                    offset: word_offset,
                    target,
                    target_kind,
                });
            }
        }
        offset += u32::from(instruction.word_len);
    }
    Ok(())
}

/// Feedback sites have stable ordering and bounds, while their mutable feedback stays isolate-local.
pub(super) fn validate_feedback_sites(
    function: FunctionId,
    feedback_sites: &[FeedbackSite],
    bytecode: &VerifiedBytecode,
    feedback_slot_count: u32,
) -> Result<(), ModuleBuildError> {
    let mut previous: Option<WordOffset> = None;
    for &site in feedback_sites {
        if !bytecode.is_instruction_start(site.offset) {
            return Err(ModuleBuildError::FeedbackSiteOffsetNotInstructionStart {
                function,
                offset: site.offset,
            });
        }
        if site.slot.index() >= feedback_slot_count {
            return Err(ModuleBuildError::FeedbackSlotOutOfRange {
                function,
                slot: site.slot,
                feedback_slot_count,
            });
        }
        if let Some(previous) = previous.filter(|previous| site.offset.index() <= previous.index())
        {
            return Err(ModuleBuildError::FeedbackSitesNotMonotonic {
                function,
                previous,
                current: site.offset,
            });
        }
        previous = Some(site.offset);
    }
    Ok(())
}

/// Binding plans may name future storage classes, but every currently bounded index is verified.
pub(super) fn validate_binding_plan(
    function: FunctionId,
    bindings: &[BindingPlanEntry],
    layout: FunctionLayout,
    max_environment_slot_count: u32,
    environment_slots: &[EnvironmentSlotMetadata],
) -> Result<(), ModuleBuildError> {
    for binding in bindings {
        if binding.name.is_empty() {
            return Err(ModuleBuildError::EmptyBindingName {
                function,
                binding: binding.clone(),
            });
        }
        match binding.location {
            BindingLocation::FrameRegister(register)
                if register.index() >= layout.register_count =>
            {
                return Err(ModuleBuildError::BindingRegisterOutOfRange {
                    function,
                    binding: binding.clone(),
                    register_count: layout.register_count,
                });
            }
            BindingLocation::Environment { slot, .. } if slot >= max_environment_slot_count => {
                return Err(ModuleBuildError::BindingEnvironmentSlotOutOfRange {
                    function,
                    binding: binding.clone(),
                    environment_slot_count: max_environment_slot_count,
                });
            }
            _ => {}
        }
    }
    for binding in bindings {
        let BindingLocation::Environment { depth: 0, slot } = binding.location else {
            continue;
        };
        if layout.environment_slot_count == 0 {
            continue;
        }
        let Some(owner) = environment_slots.get(slot as usize) else {
            return Err(ModuleBuildError::EnvironmentSlotBindingMismatch {
                function,
                binding: binding.clone(),
            });
        };
        if owner.name != binding.name || owner.mutable != binding.mutable {
            return Err(ModuleBuildError::EnvironmentSlotBindingMismatch {
                function,
                binding: binding.clone(),
            });
        }
    }
    Ok(())
}

/// Environment owner metadata is an exact dense slice indexed by direct slot operands.
pub(super) fn validate_environment_slots(
    function: FunctionId,
    slots: &[EnvironmentSlotMetadata],
    layout: FunctionLayout,
) -> Result<(), ModuleBuildError> {
    let actual = u32::try_from(slots.len()).unwrap_or(u32::MAX);
    if actual != layout.environment_slot_count {
        return Err(ModuleBuildError::EnvironmentSlotMetadataCountMismatch {
            function,
            expected: layout.environment_slot_count,
            actual,
        });
    }
    for (index, slot) in slots.iter().enumerate() {
        if slot.name.is_empty() {
            return Err(ModuleBuildError::EmptyEnvironmentSlotName {
                function,
                slot: u32::try_from(index).expect("validated environment slot count fits u32"),
            });
        }
    }
    if let Some(slot) = layout.self_binding_slot {
        let Some(metadata) = slots.get(slot as usize) else {
            return Err(ModuleBuildError::InvalidSelfBindingSlot { function, slot });
        };
        if metadata.mutable || !metadata.initialized {
            return Err(ModuleBuildError::InvalidSelfBindingSlot { function, slot });
        }
    }
    Ok(())
}

/// Suspend metadata gives a resumed fiber enough information to restore state without replaying bytecode.
pub(super) fn validate_suspend_points(
    function: FunctionId,
    suspend_points: &[SuspendPoint],
    bytecode: &VerifiedBytecode,
    register_count: u32,
    kind: FunctionKind,
) -> Result<(), ModuleBuildError> {
    for (index, &suspend_point) in suspend_points.iter().enumerate() {
        let expected = SuspendPointId::new(u32::try_from(index).map_err(|_| {
            ModuleBuildError::InvalidSuspendPoint {
                function,
                suspend_point,
            }
        })?);
        if suspend_point.id != expected {
            return Err(ModuleBuildError::SuspendPointIdMismatch {
                function,
                expected,
                actual: suspend_point.id,
            });
        }
        if !bytecode.is_instruction_start(suspend_point.instruction)
            || !bytecode.is_instruction_start(suspend_point.resume_offset)
            || suspend_point.destination.index() >= register_count
        {
            return Err(ModuleBuildError::InvalidSuspendPoint {
                function,
                suspend_point,
            });
        }
        let instruction =
            decode_instruction(bytecode.bytecode().words(), suspend_point.instruction).map_err(
                |_| ModuleBuildError::VerifiedBytecodeDecodeInvariant {
                    function,
                    offset: suspend_point.instruction,
                },
            )?;
        if !matches!(instruction.opcode, Opcode::Await | Opcode::Yield)
            || instruction.operands[1] != suspend_point.destination.index()
            || instruction.operands[2] != suspend_point.id.index()
            || suspend_point.resume_offset.index()
                != suspend_point.instruction.index() + u32::from(instruction.word_len)
        {
            return Err(ModuleBuildError::InvalidSuspendPoint {
                function,
                suspend_point,
            });
        }
    }
    validate_suspend_opcodes(function, kind, bytecode, suspend_points)
}

/// Every suspend opcode must name metadata with a compatible function kind so resume is deterministic.
fn validate_suspend_opcodes(
    function: FunctionId,
    kind: FunctionKind,
    bytecode: &VerifiedBytecode,
    suspend_points: &[SuspendPoint],
) -> Result<(), ModuleBuildError> {
    let words = bytecode.bytecode().words();
    let mut offset = 0u32;
    while (offset as usize) < words.len() {
        let word_offset = WordOffset::new(offset);
        let decoded = decode_instruction(words, word_offset).map_err(|_| {
            ModuleBuildError::VerifiedBytecodeDecodeInvariant {
                function,
                offset: word_offset,
            }
        })?;
        if matches!(decoded.opcode, Opcode::Await | Opcode::Yield) {
            let is_compatible = match decoded.opcode {
                Opcode::Await => matches!(
                    kind,
                    FunctionKind::Module | FunctionKind::Async | FunctionKind::AsyncGenerator
                ),
                Opcode::Yield => {
                    matches!(kind, FunctionKind::Generator | FunctionKind::AsyncGenerator)
                }
                _ => false,
            };
            if !is_compatible {
                return Err(ModuleBuildError::SuspendInIncompatibleFunction {
                    function,
                    kind,
                    offset: word_offset,
                    opcode: decoded.opcode,
                });
            }
            let id = SuspendPointId::new(decoded.operands[2]);
            if suspend_points
                .get(id.index() as usize)
                .map(|point| point.id)
                != Some(id)
            {
                return Err(ModuleBuildError::SuspendPointMissing {
                    function,
                    offset: word_offset,
                    id,
                });
            }
        }
        offset += u32::from(decoded.word_len);
    }
    Ok(())
}

/// Verifies the complete stream in two passes so jumps can only land on decoded instruction boundaries.
pub(super) fn verify(
    bytecode: Bytecode,
    context: VerifyContext,
) -> Result<VerifiedBytecode, VerifyError> {
    let words = bytecode.words();
    if words.len() > u32::MAX as usize {
        return Err(VerifyError::CodeTooLarge {
            word_count: words.len(),
        });
    }
    let mut starts = vec![false; words.len()];
    let mut offsets = Vec::new();
    let mut offset = 0usize;
    let mut last_opcode = None;
    while offset < words.len() {
        let word_offset = WordOffset::new(offset as u32);
        let decoded = decode_instruction(words, word_offset).map_err(VerifyError::Decode)?;
        starts[offset] = true;
        offsets.push((word_offset, decoded));
        last_opcode = Some(decoded.opcode);
        offset += decoded.word_len as usize;
    }
    if !matches!(last_opcode, Some(opcode) if opcode.is_terminal()) {
        return Err(VerifyError::MissingTerminalInstruction);
    }
    for (offset, decoded) in offsets {
        verify_instruction(decoded, offset, &starts, context)?;
    }
    Ok(VerifiedBytecode {
        bytecode,
        starts: starts.into(),
    })
}

/// Checks operand domains after the first pass has established every instruction boundary.
fn verify_instruction(
    instruction: DecodedInstruction,
    offset: WordOffset,
    starts: &[bool],
    context: VerifyContext,
) -> Result<(), VerifyError> {
    let operands = instruction.operands;
    let check_register = |register| {
        if register < context.register_count {
            Ok(())
        } else {
            Err(VerifyError::RegisterOutOfRange {
                offset,
                register,
                register_count: context.register_count,
            })
        }
    };
    match instruction.opcode {
        Opcode::Nop
        | Opcode::Jump
        | Opcode::EnterFinally
        | Opcode::ResumeCompletion
        | Opcode::BreakThroughFinally
        | Opcode::ContinueThroughFinally
        | Opcode::ReturnUndefined
        | Opcode::DeclareScope
        | Opcode::DeclareGlobalLexical => {}
        Opcode::LoadUndefined
        | Opcode::LoadNull
        | Opcode::LoadFalse
        | Opcode::LoadTrue
        | Opcode::LoadImmediate
        | Opcode::LoadConstant
        | Opcode::LoadScope => check_register(operands[0])?,
        Opcode::CreateObject
        | Opcode::CreateArray
        | Opcode::LoadException
        | Opcode::LoadThis
        | Opcode::LoadNewTarget
        | Opcode::LoadArgumentsLength
        | Opcode::LoadArgumentsObject
        | Opcode::InitializeThis
        | Opcode::CheckConstructor => check_register(operands[0])?,
        Opcode::Move => {
            check_register(operands[0])?;
            check_register(operands[1])?;
        }
        Opcode::Not
        | Opcode::Negate
        | Opcode::Typeof
        | Opcode::ToNumber
        | Opcode::BitwiseNot
        | Opcode::TypeofScope
        | Opcode::CreateForInIterator
        | Opcode::ForInNext
        | Opcode::ExcludePropertyKey
        | Opcode::CollectRestArguments => {
            check_register(operands[0])?;
            check_register(operands[1])?;
        }
        Opcode::CreateExclusionList => check_register(operands[0])?,
        Opcode::SetFunctionName => check_register(operands[0])?,
        Opcode::Add
        | Opcode::Sub
        | Opcode::Mul
        | Opcode::Div
        | Opcode::StrictEqual
        | Opcode::LessThan
        | Opcode::BitwiseAnd
        | Opcode::BitwiseOr
        | Opcode::BitwiseXor
        | Opcode::ShiftLeft
        | Opcode::ShiftRight
        | Opcode::ShiftRightUnsigned
        | Opcode::Remainder
        | Opcode::Exponentiate
        | Opcode::GreaterThan
        | Opcode::LessEqual
        | Opcode::GreaterEqual
        | Opcode::LooseEqual
        | Opcode::LooseNotEqual
        | Opcode::HasProperty
        | Opcode::DeleteById
        | Opcode::DeleteByValue
        | Opcode::InstanceOf
        | Opcode::GetByValue
        | Opcode::SetByValue
        | Opcode::ToPropertyKey
        | Opcode::ToPropertyKeyForIn
        | Opcode::DefineGetterById
        | Opcode::DefineSetterById
        | Opcode::DefineGetterByValue
        | Opcode::DefineSetterByValue
        | Opcode::CopyDataProperties => {
            check_register(operands[0])?;
            check_register(operands[1])?;
            check_register(operands[2])?;
        }
        Opcode::SetAccessorFunctionName => {
            check_register(operands[0])?;
            check_register(operands[1])?;
            if operands[2] > 1 {
                return Err(VerifyError::InvalidBooleanOperand {
                    offset,
                    operand: operands[2],
                });
            }
        }
        Opcode::LoadEnvironment | Opcode::StoreEnvironment => check_register(operands[0])?,
        Opcode::GetById | Opcode::SetById => {
            check_register(operands[0])?;
            check_register(operands[1])?;
        }
        Opcode::Call | Opcode::Construct | Opcode::SuperConstruct => {
            check_register(operands[0])?;
            check_register(operands[1])?;
            if operands[1]
                .checked_add(operands[2])
                .is_none_or(|last_argument| last_argument >= context.register_count)
                && operands[2] != 0
            {
                return Err(VerifyError::InvalidCallArgumentWindow {
                    offset,
                    callee: operands[1],
                    argument_count: operands[2],
                    register_count: context.register_count,
                });
            }
        }
        Opcode::CallWithReceiver => {
            check_register(operands[0])?;
            check_register(operands[1])?;
            if operands[1]
                .checked_add(1)
                .and_then(|callee| callee.checked_add(operands[2]))
                .is_none_or(|last_argument| last_argument >= context.register_count)
            {
                return Err(VerifyError::InvalidCallArgumentWindow {
                    offset,
                    callee: operands[1].saturating_add(1),
                    argument_count: operands[2],
                    register_count: context.register_count,
                });
            }
        }
        Opcode::Await | Opcode::Yield => {
            check_register(operands[0])?;
            check_register(operands[1])?;
        }
        Opcode::JumpIfFalse | Opcode::JumpIfTrue | Opcode::JumpIfNotNullish => {
            check_register(operands[0])?
        }
        Opcode::Return | Opcode::Throw => check_register(operands[0])?,
        Opcode::CreateClosure => check_register(operands[0])?,
        Opcode::CreateClass => {
            check_register(operands[0])?;
            check_register(operands[2])?;
            check_register(
                operands[2]
                    .checked_add(1)
                    .ok_or(VerifyError::RegisterOutOfRange {
                        offset,
                        register: u32::MAX,
                        register_count: context.register_count,
                    })?,
            )?;
        }
        Opcode::StoreScope | Opcode::StoreResolvedScope | Opcode::InitializeGlobalLexical => {
            check_register(operands[0])?
        }
    }
    if instruction.opcode == Opcode::LoadConstant && operands[1] >= context.constant_count {
        return Err(VerifyError::ConstantOutOfRange {
            offset,
            constant: operands[1],
            constant_count: context.constant_count,
        });
    }
    let scope_name = match instruction.opcode {
        Opcode::LoadScope | Opcode::StoreScope | Opcode::StoreResolvedScope => Some(operands[1]),
        Opcode::DeclareScope => Some(operands[0]),
        Opcode::DeclareGlobalLexical => Some(operands[0]),
        Opcode::InitializeGlobalLexical => Some(operands[1]),
        Opcode::GetById | Opcode::SetById | Opcode::DefineGetterById | Opcode::DefineSetterById => {
            Some(operands[2])
        }
        _ => None,
    };
    if scope_name.is_some_and(|scope_name| scope_name >= context.scope_name_count) {
        return Err(VerifyError::ScopeNameOutOfRange {
            offset,
            scope_name: scope_name.expect("scope-name opcode selected above"),
            scope_name_count: context.scope_name_count,
        });
    }
    if matches!(
        instruction.opcode,
        Opcode::CreateClosure | Opcode::CreateClass
    ) && operands[1] >= context.function_count
    {
        return Err(VerifyError::FunctionOutOfRange {
            offset,
            function: operands[1],
            function_count: context.function_count,
        });
    }
    if matches!(
        instruction.opcode,
        Opcode::LoadEnvironment | Opcode::StoreEnvironment
    ) && operands[2] >= context.max_environment_slot_count
    {
        return Err(VerifyError::EnvironmentSlotOutOfRange {
            offset,
            slot: operands[2],
            max_environment_slot_count: context.max_environment_slot_count,
        });
    }
    if instruction.opcode == Opcode::DeclareGlobalLexical && operands[1] > 1 {
        return Err(VerifyError::InvalidBooleanOperand {
            offset,
            operand: operands[1],
        });
    }
    if matches!(
        instruction.opcode,
        Opcode::Jump
            | Opcode::BreakThroughFinally
            | Opcode::ContinueThroughFinally
            | Opcode::JumpIfFalse
            | Opcode::JumpIfTrue
            | Opcode::JumpIfNotNullish
    ) && !starts
        .get(if matches!(
            instruction.opcode,
            Opcode::Jump | Opcode::BreakThroughFinally | Opcode::ContinueThroughFinally
        ) {
            operands[0]
        } else {
            operands[1]
        } as usize)
        .copied()
        .unwrap_or(false)
    {
        return Err(VerifyError::InvalidJumpTarget {
            offset,
            target: if instruction.opcode == Opcode::Jump {
                operands[0]
            } else {
                operands[1]
            },
        });
    }
    Ok(())
}
