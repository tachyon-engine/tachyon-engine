#![deny(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stderr,
    clippy::print_stdout,
    unsafe_op_in_unsafe_fn
)]
//! Isolate, fiber, interpreter, and ECMAScript builtin execution machinery.
//!
//! This crate intentionally has no host I/O surface.

use core::cell::Cell;

use tachyon_bytecode::{
    BytecodeConstant, CompiledModule, FunctionId, Opcode, RegisterId, WordOffset,
    decode_instruction,
};
use tachyon_value::{Immediate, Value};

/// Shareable immutable engine configuration. Host services deliberately do not live here.
#[derive(Clone, Copy, Debug, Default)]
pub struct Engine;

/// A per-execution bound; fuel is a hard cap while quantum bounds one synchronous interpreter turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionBudget {
    pub fuel: u64,
    pub quantum: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunOutcome {
    Completed(Value),
    BudgetExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionError {
    MissingEntryFunction(FunctionId),
    RegisterWindowTooLarge(u32),
    RegisterAllocationFailed,
    DecodeInvariant(WordOffset),
    UnsupportedOpcode(Opcode),
    UnsupportedConstant(u32),
    InvalidRegister(RegisterId),
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    function: FunctionId,
    pc: WordOffset,
    base: u32,
}

#[derive(Debug, Default)]
struct Fiber {
    frames: Vec<Frame>,
    registers: Vec<Value>,
}

/// A single-thread-owned ECMAScript execution state; `Cell` intentionally makes it `!Sync`.
#[derive(Debug)]
pub struct Isolate {
    fiber: Fiber,
    _not_sync: Cell<()>,
}

impl Default for Isolate {
    fn default() -> Self {
        Self {
            fiber: Fiber::default(),
            _not_sync: Cell::new(()),
        }
    }
}

impl Isolate {
    /// Starts the module entry function with one checked register reservation before opcode dispatch.
    pub fn execute(
        &mut self,
        module: &CompiledModule,
        budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        self.execute_with_batch::<1>(module, budget)
    }

    /// Executes with a fixed internal batch size so each monomorphization preserves the same fuel contract.
    fn execute_with_batch<const N: usize>(
        &mut self,
        module: &CompiledModule,
        mut budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        self.enter(module, module.entry_function())?;
        loop {
            if budget.fuel == 0 || budget.quantum == 0 {
                return Ok(RunOutcome::BudgetExhausted);
            }
            if let Some(outcome) = self.execute_batch::<N>(module, &mut budget)? {
                return Ok(outcome);
            }
        }
    }

    /// The `N` parameter is an internal dispatch-tuning knob; every executed opcode still consumes exact fuel.
    #[inline(always)]
    fn execute_batch<const N: usize>(
        &mut self,
        module: &CompiledModule,
        budget: &mut ExecutionBudget,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        for _ in 0..N {
            if budget.fuel == 0 || budget.quantum == 0 {
                return Ok(Some(RunOutcome::BudgetExhausted));
            }
            let frame = *self
                .fiber
                .frames
                .last()
                .expect("entry frame exists while executing");
            let function = module
                .function(frame.function)
                .ok_or(ExecutionError::MissingEntryFunction(frame.function))?;
            let instruction = decode_instruction(function.bytecode().bytecode().words(), frame.pc)
                .map_err(|_| ExecutionError::DecodeInvariant(frame.pc))?;
            let next_pc = WordOffset::new(frame.pc.index() + u32::from(instruction.word_len));
            self.fiber
                .frames
                .last_mut()
                .expect("frame remains active")
                .pc = next_pc;
            budget.fuel -= 1;
            budget.quantum -= 1;
            if let Some(outcome) =
                self.dispatch(module, instruction.opcode, instruction.operands, frame.base)?
            {
                return Ok(Some(outcome));
            }
        }
        Ok(None)
    }

    /// Implements the arithmetic subset emitted by the current compiler; later opcodes extend this match.
    #[inline(always)]
    fn dispatch(
        &mut self,
        module: &CompiledModule,
        opcode: Opcode,
        operands: [u32; 3],
        base: u32,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match opcode {
            Opcode::LoadUndefined => self.write(
                base,
                operands[0],
                Value::from_immediate(Immediate::Undefined),
            )?,
            Opcode::LoadNull => {
                self.write(base, operands[0], Value::from_immediate(Immediate::Null))?
            }
            Opcode::LoadFalse => {
                self.write(base, operands[0], Value::from_immediate(Immediate::False))?
            }
            Opcode::LoadTrue => {
                self.write(base, operands[0], Value::from_immediate(Immediate::True))?
            }
            Opcode::LoadImmediate => {
                self.write(base, operands[0], Value::from_i32(operands[1] as i32))?
            }
            Opcode::LoadConstant => {
                let constant = module
                    .constants()
                    .get(operands[1] as usize)
                    .ok_or(ExecutionError::UnsupportedConstant(operands[1]))?;
                let value = match constant {
                    BytecodeConstant::NumberBits(bits) => Value::from_f64(f64::from_bits(*bits)),
                    _ => return Err(ExecutionError::UnsupportedConstant(operands[1])),
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::Move => {
                let value = self.read(base, operands[1])?;
                self.write(base, operands[0], value)?;
            }
            Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::Div => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                self.write(base, operands[0], numeric_binary(opcode, left, right))?;
            }
            Opcode::StrictEqual => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                let value = if strict_equal(left, right) {
                    Value::from_immediate(Immediate::True)
                } else {
                    Value::from_immediate(Immediate::False)
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::Return => {
                return Ok(Some(RunOutcome::Completed(self.read(base, operands[0])?)));
            }
            _ => return Err(ExecutionError::UnsupportedOpcode(opcode)),
        }
        Ok(None)
    }

    fn enter(
        &mut self,
        module: &CompiledModule,
        function_id: FunctionId,
    ) -> Result<(), ExecutionError> {
        let function = module
            .function(function_id)
            .ok_or(ExecutionError::MissingEntryFunction(function_id))?;
        let register_count = usize::try_from(function.layout().register_count).map_err(|_| {
            ExecutionError::RegisterWindowTooLarge(function.layout().register_count)
        })?;
        self.fiber.frames.clear();
        self.fiber.registers.clear();
        self.fiber
            .frames
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        self.fiber
            .registers
            .try_reserve_exact(register_count)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        self.fiber
            .registers
            .resize(register_count, Value::from_immediate(Immediate::Undefined));
        self.fiber.frames.push(Frame {
            function: function_id,
            pc: WordOffset::new(0),
            base: 0,
        });
        Ok(())
    }

    #[inline(always)]
    fn read(&self, base: u32, register: u32) -> Result<Value, ExecutionError> {
        self.fiber
            .registers
            .get(base as usize + register as usize)
            .copied()
            .ok_or(ExecutionError::InvalidRegister(RegisterId::new(register)))
    }

    #[inline(always)]
    fn write(&mut self, base: u32, register: u32, value: Value) -> Result<(), ExecutionError> {
        let slot = self
            .fiber
            .registers
            .get_mut(base as usize + register as usize)
            .ok_or(ExecutionError::InvalidRegister(RegisterId::new(register)))?;
        *slot = value;
        Ok(())
    }
}

#[inline(always)]
fn numeric_binary(opcode: Opcode, left: Value, right: Value) -> Value {
    if let (Some(left), Some(right)) = (left.as_i32(), right.as_i32()) {
        let integer = match opcode {
            Opcode::Add => left.checked_add(right),
            Opcode::Sub => left.checked_sub(right),
            Opcode::Mul => left.checked_mul(right),
            Opcode::Div if right != 0 && left % right == 0 => left.checked_div(right),
            _ => None,
        };
        if let Some(integer) = integer {
            return Value::from_i32(integer);
        }
    }
    let left_number = left
        .as_i32()
        .map_or_else(|| left.as_f64().unwrap_or(f64::NAN), f64::from);
    let right_number = right
        .as_i32()
        .map_or_else(|| right.as_f64().unwrap_or(f64::NAN), f64::from);
    Value::from_f64(match opcode {
        Opcode::Add => left_number + right_number,
        Opcode::Sub => left_number - right_number,
        Opcode::Mul => left_number * right_number,
        Opcode::Div => left_number / right_number,
        _ => unreachable!("numeric binary dispatch only supplies arithmetic opcodes"),
    })
}

#[inline(always)]
fn strict_equal(left: Value, right: Value) -> bool {
    match (numeric_value(left), numeric_value(right)) {
        (Some(left), Some(right)) => left == right,
        _ => left == right,
    }
}

#[inline(always)]
fn numeric_value(value: Value) -> Option<f64> {
    value.as_i32().map(f64::from).or_else(|| value.as_f64())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tachyon_bytecode::{
        Bytecode, CompiledFunctionTemplate, CompiledModule, FunctionId, FunctionKind,
        FunctionLayout, FunctionMetadata, encode_instruction,
    };

    use super::*;

    fn arithmetic_module() -> CompiledModule {
        let mut words = encode_instruction(Opcode::LoadImmediate, &[0, 1]).unwrap();
        words.extend(encode_instruction(Opcode::LoadImmediate, &[1, 2]).unwrap());
        words.extend(encode_instruction(Opcode::Add, &[2, 0, 1]).unwrap());
        words.extend(encode_instruction(Opcode::Return, &[2]).unwrap());
        let metadata = FunctionMetadata::new(
            FunctionKind::Script,
            FunctionLayout {
                register_count: 3,
                ..FunctionLayout::default()
            },
        );
        CompiledModule::new(
            Arc::from("1 + 2"),
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

    #[test]
    fn interpreter_executes_int32_arithmetic() {
        assert_batch_result::<1>();
        assert_batch_result::<2>();
        assert_batch_result::<4>();
        assert_batch_result::<8>();
        assert_batch_result::<16>();
    }

    #[test]
    fn interpreter_stops_at_exact_budget_boundary() {
        assert_batch_budget::<1>();
        assert_batch_budget::<2>();
        assert_batch_budget::<4>();
        assert_batch_budget::<8>();
        assert_batch_budget::<16>();
    }

    fn assert_batch_result<const N: usize>() {
        let outcome = Isolate::default()
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

    fn assert_batch_budget<const N: usize>() {
        let outcome = Isolate::default()
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
}
