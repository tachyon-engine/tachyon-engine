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
    BytecodeConstant, CompiledModule, FunctionId, FunctionKind, Opcode, RegisterId, WordOffset,
    decode_instruction,
};
use tachyon_gc::{GcRef, Trace, Tracer};
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
    Thrown(Value),
    BudgetExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionError {
    MissingEntryFunction(FunctionId),
    RegisterWindowTooLarge(u32),
    HandlerStackTooLarge(u32),
    CompletionStackTooLarge(u32),
    FrameAllocationFailed,
    RegisterAllocationFailed,
    HandlerAllocationFailed,
    CompletionAllocationFailed,
    DecodeInvariant(WordOffset),
    UnsupportedOpcode(Opcode),
    UnsupportedConstant(u32),
    InvalidRegister(RegisterId),
}

/// A future GC-managed lexical environment. Its concrete payload arrives with M5.
#[derive(Debug)]
struct Environment;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Strictness {
    Sloppy,
    Strict,
}

/// One explicit JavaScript activation. Rust stack frames never represent JavaScript calls.
#[derive(Clone, Copy, Debug)]
struct Frame {
    function: FunctionId,
    pc: WordOffset,
    base: u32,
    environment: Option<GcRef<Environment>>,
    return_register: Option<RegisterId>,
    this_value: Value,
    new_target: Value,
    strictness: Strictness,
}

/// The dynamic handler state selected from immutable bytecode handler metadata.
#[derive(Clone, Copy, Debug)]
struct ActiveHandler {
    handler_index: u32,
    frame_depth: u32,
    environment_depth: u32,
}

/// Abrupt completions are data, so throw/finally never need Rust stack unwinding.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // Populated by Throw/finally lowering after handler dispatch is implemented.
enum Completion {
    Return(Value),
    Throw(Value),
}

#[derive(Debug, Default)]
struct Fiber {
    frames: Vec<Frame>,
    registers: Vec<Value>,
    handlers: Vec<ActiveHandler>,
    completions: Vec<Completion>,
}

impl Fiber {
    /// Traces every mutable reference reachable from an active, yielded, or suspended fiber.
    ///
    /// Frame control indices are validated when handlers are installed. They do not themselves
    /// own heap references, while registers, frame context, and abrupt completion payloads do.
    fn trace_roots(&mut self, tracer: &mut dyn Tracer) {
        self.registers.trace(tracer);
        for frame in &mut self.frames {
            frame.environment.trace(tracer);
            frame.this_value.trace(tracer);
            frame.new_target.trace(tracer);
            if let Some(return_register) = frame.return_register {
                debug_assert!((return_register.index() as usize) < self.registers.len());
            }
            let _is_strict = matches!(frame.strictness, Strictness::Strict);
        }
        for handler in &self.handlers {
            debug_assert!(
                usize::try_from(handler.frame_depth).is_ok_and(|depth| depth <= self.frames.len())
            );
            debug_assert!(
                usize::try_from(handler.environment_depth)
                    .is_ok_and(|depth| depth <= self.frames.len())
            );
            let _ = handler.handler_index;
        }
        for completion in &mut self.completions {
            match completion {
                Completion::Return(value) | Completion::Throw(value) => value.trace(tracer),
            }
        }
    }
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
    /// Enumerates this isolate's fiber roots for a stop-the-world collection safepoint.
    ///
    /// The collector supplies a rewrite-capable tracer. This API does not resolve cage offsets or
    /// borrow heap objects, so it remains valid for both the phase-1 mark-sweep heap and moving GC.
    pub fn trace_roots(&mut self, tracer: &mut dyn Tracer) {
        self.fiber.trace_roots(tracer);
    }

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
            Opcode::Not => {
                let value = if is_truthy(self.read(base, operands[1])?) {
                    Value::from_immediate(Immediate::False)
                } else {
                    Value::from_immediate(Immediate::True)
                };
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
            Opcode::Jump => self.set_pc(WordOffset::new(operands[0])),
            Opcode::JumpIfFalse => {
                if !is_truthy(self.read(base, operands[0])?) {
                    self.set_pc(WordOffset::new(operands[1]));
                }
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
        self.fiber.handlers.clear();
        self.fiber.completions.clear();
        self.reserve_entry_state(function.layout(), register_count)?;
        self.fiber
            .registers
            .resize(register_count, Value::from_immediate(Immediate::Undefined));
        self.fiber.frames.push(Frame {
            function: function_id,
            pc: WordOffset::new(0),
            base: 0,
            environment: None,
            return_register: None,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: Value::from_immediate(Immediate::Undefined),
            strictness: strictness_for(function.kind()),
        });
        Ok(())
    }

    /// Reserves the exact entry-function execution windows before any opcode can push into them.
    ///
    /// Calls will extend these windows from the callee's verified metadata in a later VM package.
    /// Until then, reserving the handler and completion depths here proves the no-reallocation
    /// contract without conflating bytecode decoding with collection growth policy.
    fn reserve_entry_state(
        &mut self,
        layout: tachyon_bytecode::FunctionLayout,
        register_count: usize,
    ) -> Result<(), ExecutionError> {
        let handler_depth = usize::try_from(layout.max_handler_depth)
            .map_err(|_| ExecutionError::HandlerStackTooLarge(layout.max_handler_depth))?;
        let completion_depth = usize::try_from(layout.max_completion_depth)
            .map_err(|_| ExecutionError::CompletionStackTooLarge(layout.max_completion_depth))?;
        self.fiber
            .frames
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::FrameAllocationFailed)?;
        self.fiber
            .registers
            .try_reserve_exact(register_count)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        self.fiber
            .handlers
            .try_reserve_exact(handler_depth)
            .map_err(|_| ExecutionError::HandlerAllocationFailed)?;
        self.fiber
            .completions
            .try_reserve_exact(completion_depth)
            .map_err(|_| ExecutionError::CompletionAllocationFailed)?;
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

    #[inline(always)]
    fn set_pc(&mut self, pc: WordOffset) {
        self.fiber
            .frames
            .last_mut()
            .expect("frame remains active while jumping")
            .pc = pc;
    }
}

/// Modules are intrinsically strict; other function kinds await lowering-provided strict metadata.
#[inline]
fn strictness_for(kind: FunctionKind) -> Strictness {
    if matches!(kind, FunctionKind::Module) {
        Strictness::Strict
    } else {
        Strictness::Sloppy
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

#[inline(always)]
fn is_truthy(value: Value) -> bool {
    if let Some(integer) = value.as_i32() {
        return integer != 0;
    }
    if let Some(number) = value.as_f64() {
        return number != 0.0 && !number.is_nan();
    }
    !matches!(
        value.as_immediate(),
        Some(Immediate::Undefined | Immediate::Null | Immediate::False)
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tachyon_bytecode::{
        Bytecode, BytecodeBuilder, CompiledFunctionTemplate, CompiledModule, FunctionId,
        FunctionKind, FunctionLayout, FunctionMetadata, SourceSpan, encode_instruction,
    };
    use tachyon_gc::{GcRef, Tracer};
    use tachyon_value::RawHeapRef;

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

    #[test]
    fn interpreter_restarts_dispatch_after_conditional_jumps() {
        assert_conditional_batch::<1>();
        assert_conditional_batch::<2>();
        assert_conditional_batch::<4>();
        assert_conditional_batch::<8>();
        assert_conditional_batch::<16>();
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
        let mut isolate = Isolate::default();
        isolate
            .enter(
                &state_module(FunctionKind::Module, layout),
                FunctionId::new(0),
            )
            .unwrap();

        assert!(isolate.fiber.frames.capacity() >= 1);
        assert!(isolate.fiber.registers.capacity() >= 2);
        assert!(isolate.fiber.handlers.capacity() >= 3);
        assert!(isolate.fiber.completions.capacity() >= 4);
        assert_eq!(isolate.fiber.frames[0].strictness, Strictness::Strict);
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
        let mut isolate = Isolate::default();
        isolate
            .enter(
                &state_module(FunctionKind::Script, layout),
                FunctionId::new(0),
            )
            .unwrap();
        let raw = RawHeapRef::new(16).expect("non-zero cage offset");
        isolate.fiber.registers[0] = Value::from_heap_ref(raw);
        let frame = isolate.fiber.frames.last_mut().expect("entry frame exists");
        frame.environment = Some(GcRef::from_raw(raw));
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
        let mut tracer = RewritingTracer;

        isolate.trace_roots(&mut tracer);

        let rewritten = RawHeapRef::new(32).expect("non-zero cage offset");
        assert_eq!(isolate.fiber.registers[0].as_heap_ref(), Some(rewritten));
        let frame = isolate.fiber.frames.last().expect("entry frame exists");
        assert_eq!(frame.environment.map(GcRef::raw), Some(rewritten));
        assert_eq!(frame.this_value.as_heap_ref(), Some(rewritten));
        assert_eq!(frame.new_target.as_heap_ref(), Some(rewritten));
        assert!(matches!(
            isolate.fiber.completions[0],
            Completion::Return(value) if value.as_heap_ref() == Some(rewritten)
        ));
        assert!(matches!(
            isolate.fiber.completions[1],
            Completion::Throw(value) if value.as_heap_ref() == Some(rewritten)
        ));
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
    }

    fn rewrite(reference: RawHeapRef) -> RawHeapRef {
        RawHeapRef::new(reference.offset() + 16).expect("test offset stays non-zero")
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

    fn assert_conditional_batch<const N: usize>() {
        for (test, expected) in [(Opcode::LoadTrue, 1), (Opcode::LoadFalse, 2)] {
            let outcome = Isolate::default()
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
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
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
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                Bytecode::from_words(words),
                FunctionMetadata::new(kind, layout),
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }
}
