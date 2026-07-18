//! Text rendering for verified bytecode, intended for diagnostics and compiler tests.

use core::fmt::{self, Write};

use crate::{CompiledFunction, DecodeError, Opcode, WordOffset, decode_instruction};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisassemblyError {
    VerifiedBytecodeDecodeInvariant {
        offset: WordOffset,
        error: DecodeError,
    },
    Formatting,
}

/// Renders verified instructions with logical operands, source spans, and immutable feedback sites.
pub fn disassemble(function: &CompiledFunction) -> Result<String, DisassemblyError> {
    let bytecode = function.bytecode().bytecode();
    let words = bytecode.words();
    let source_map = function.source_map();
    let feedback_sites = function.feedback_sites();
    let mut source_index = 0usize;
    let mut feedback_index = 0usize;
    let mut offset = 0u32;
    let mut output = String::new();

    while (offset as usize) < words.len() {
        let word_offset = WordOffset::new(offset);
        let instruction = decode_instruction(words, word_offset).map_err(|error| {
            DisassemblyError::VerifiedBytecodeDecodeInvariant {
                offset: word_offset,
                error,
            }
        })?;
        write!(&mut output, "{offset:06} ").map_err(|_| DisassemblyError::Formatting)?;
        if source_map.get(source_index).map(|entry| entry.offset) == Some(word_offset) {
            let span = source_map[source_index].span;
            write!(&mut output, "[{}..{}] ", span.start, span.end)
                .map_err(|_| DisassemblyError::Formatting)?;
            source_index += 1;
        } else {
            output.push_str("[--] ");
        }
        write_instruction(&mut output, instruction.opcode, &instruction.operands)
            .map_err(|_| DisassemblyError::Formatting)?;
        if feedback_sites.get(feedback_index).map(|site| site.offset) == Some(word_offset) {
            write!(
                &mut output,
                " feedback={}",
                feedback_sites[feedback_index].slot.index()
            )
            .map_err(|_| DisassemblyError::Formatting)?;
            feedback_index += 1;
        }
        output.push('\n');
        offset += u32::from(instruction.word_len);
    }
    Ok(output)
}

/// Renders operands by role instead of exposing compact/normal/wide physical encoding details.
fn write_instruction(output: &mut String, opcode: Opcode, operands: &[u32; 3]) -> fmt::Result {
    write!(output, "{opcode}")?;
    match opcode {
        Opcode::Nop | Opcode::ReturnUndefined => {}
        Opcode::DeclareScope => write!(output, " scope={}", operands[0])?,
        Opcode::DeclareGlobalLexical => {
            write!(output, " scope={}, mutable={}", operands[0], operands[1])?
        }
        Opcode::LoadUndefined => write!(output, " r{}", operands[0])?,
        Opcode::CreateObject | Opcode::LoadException | Opcode::LoadThis | Opcode::LoadNewTarget => {
            write!(output, " r{}", operands[0])?
        }
        Opcode::LoadNull => write!(output, " r{}", operands[0])?,
        Opcode::LoadFalse => write!(output, " r{}", operands[0])?,
        Opcode::LoadTrue => write!(output, " r{}", operands[0])?,
        Opcode::LoadImmediate => write!(output, " r{}, imm={}", operands[0], operands[1])?,
        Opcode::LoadConstant => write!(output, " r{}, const={}", operands[0], operands[1])?,
        Opcode::Move
        | Opcode::Not
        | Opcode::Negate
        | Opcode::Typeof
        | Opcode::ToNumber
        | Opcode::BitwiseNot => write!(output, " r{}, r{}", operands[0], operands[1])?,
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
        | Opcode::TypeofScope
        | Opcode::DeleteById
        | Opcode::DeleteByValue
        | Opcode::InstanceOf
        | Opcode::GetByValue
        | Opcode::SetByValue => write!(
            output,
            " r{}, r{}, r{}",
            operands[0], operands[1], operands[2]
        )?,
        Opcode::GetById => write!(
            output,
            " r{}, receiver=r{}, name={}",
            operands[0], operands[1], operands[2]
        )?,
        Opcode::SetById => write!(
            output,
            " receiver=r{}, value=r{}, name={}",
            operands[0], operands[1], operands[2]
        )?,
        Opcode::Jump => write!(output, " pc={}", operands[0])?,
        Opcode::JumpIfFalse | Opcode::JumpIfTrue | Opcode::JumpIfNotNullish => {
            write!(output, " r{}, pc={}", operands[0], operands[1])?
        }
        Opcode::Return | Opcode::Throw => write!(output, " r{}", operands[0])?,
        Opcode::Call => write!(
            output,
            " r{}, callee=r{}, argc={}",
            operands[0], operands[1], operands[2]
        )?,
        Opcode::Construct => write!(
            output,
            " r{}, constructor=r{}, argc={}",
            operands[0], operands[1], operands[2]
        )?,
        Opcode::CallWithReceiver => write!(
            output,
            " r{}, receiver=r{}, argc={}",
            operands[0], operands[1], operands[2]
        )?,
        Opcode::CreateClosure => write!(output, " r{}, function={}", operands[0], operands[1])?,
        Opcode::LoadScope => write!(output, " r{}, scope={}", operands[0], operands[1])?,
        Opcode::StoreScope | Opcode::StoreResolvedScope => {
            write!(output, " r{}, scope={}", operands[0], operands[1])?
        }
        Opcode::LoadEnvironment | Opcode::StoreEnvironment => write!(
            output,
            " r{}, depth={}, slot={}",
            operands[0], operands[1], operands[2]
        )?,
        Opcode::InitializeGlobalLexical => {
            write!(output, " r{}, scope={}", operands[0], operands[1])?
        }
        Opcode::Await | Opcode::Yield => write!(
            output,
            " r{}, r{}, suspend={}",
            operands[0], operands[1], operands[2]
        )?,
    }
    Ok(())
}
