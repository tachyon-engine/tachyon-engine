//! Bytecode loading and explicit-fiber interpreter state machine.

use super::*;

#[inline(always)]
pub(crate) fn environment_access_error(
    depth: u32,
    slot: u32,
    error: EnvironmentAccessError,
) -> ExecutionError {
    match error {
        EnvironmentAccessError::InvalidSlot => {
            ExecutionError::InvalidEnvironmentSlot { depth, slot }
        }
        EnvironmentAccessError::Uninitialized => {
            ExecutionError::UninitializedEnvironmentBinding { depth, slot }
        }
        EnvironmentAccessError::Immutable => {
            ExecutionError::ImmutableEnvironmentBinding { depth, slot }
        }
        EnvironmentAccessError::AlreadyInitialized => {
            ExecutionError::EnvironmentBindingAlreadyInitialized { depth, slot }
        }
    }
}

impl Isolate {
    /// Enumerates this isolate's fiber roots for a stop-the-world collection safepoint.
    ///
    /// The collector supplies a rewrite-capable tracer. This API does not resolve logical addresses
    /// or borrow heap objects, so it remains valid across non-moving collection phases.
    pub fn trace_roots(&mut self, tracer: &mut dyn Tracer) {
        self.fiber.trace_roots(tracer);
        self.finalization_jobs.trace(tracer);
        self.realm.trace(tracer);
        for code in &mut self.loaded_code {
            code.trace(tracer);
        }
    }

    /// Starts the module entry function with one checked register reservation before opcode dispatch.
    pub fn execute(
        &mut self,
        module: &CompiledModule,
        budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        let code = self.load_module(module)?;
        self.execute_loaded(code, budget)
    }

    /// Resolves immutable scope names once and publishes one bounded isolate-local code entry.
    pub fn load_module(&mut self, module: &CompiledModule) -> Result<CodeId, ExecutionError> {
        if let Some(index) = self
            .loaded_code
            .iter()
            .position(|loaded| loaded.module.ptr_eq(module))
        {
            return CodeId::from_index(index)
                .ok_or(ExecutionError::LoadedModuleLimit { limit: u32::MAX });
        }
        if self.loaded_code.len() >= self.realm.limits.max_loaded_modules as usize {
            return Err(ExecutionError::LoadedModuleLimit {
                limit: self.realm.limits.max_loaded_modules,
            });
        }
        self.loaded_code
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::LoadedCodeAllocationFailed)?;
        let mut scope_resolutions = Vec::new();
        scope_resolutions
            .try_reserve_exact(module.scope_names().len())
            .map_err(|_| ExecutionError::ScopeNameAllocationFailed)?;
        let checkpoint = self.atoms.checkpoint();
        for name in module.scope_names() {
            let string = match JsString::try_from_str(name) {
                Ok(string) => string,
                Err(error) => {
                    self.atoms.rollback(checkpoint);
                    return Err(ExecutionError::ScopeNameString(error));
                }
            };
            match self.atoms.try_intern(string) {
                Ok(atom) => scope_resolutions.push(ScopeResolution {
                    atom,
                    lexical_slot: self.realm.resolve_lexical(atom),
                    intrinsic_slot: self.realm.resolve_intrinsic(atom),
                    global_slot: self.realm.resolve(atom),
                }),
                Err(error) => {
                    self.atoms.rollback(checkpoint);
                    return Err(ExecutionError::ScopeNameAtom(error));
                }
            }
        }
        let mut constant_values = Vec::new();
        if constant_values
            .try_reserve_exact(module.constants().len())
            .is_err()
        {
            self.atoms.rollback(checkpoint);
            return Err(ExecutionError::ConstantValueAllocationFailed);
        }
        for constant in module.constants() {
            let value = match constant {
                BytecodeConstant::String(code_units) => {
                    let string = match JsString::try_from_utf16(code_units) {
                        Ok(string) => string,
                        Err(error) => {
                            self.atoms.rollback(checkpoint);
                            return Err(ExecutionError::ConstantString(error));
                        }
                    };
                    let mut roots = CodeLoadRoots {
                        vm: VmRoots {
                            fiber: &mut self.fiber,
                            finalization_jobs: &mut self.finalization_jobs,
                            realm: &mut self.realm,
                            loaded_code: &mut self.loaded_code,
                        },
                        constant_values: &mut constant_values,
                    };
                    match self.heap.try_allocate_external_with_gc(
                        self.types.string,
                        0,
                        string,
                        AllocationSpace::Young,
                        &mut roots,
                    ) {
                        Ok(reference) => Some(Value::from_heap_ref(reference.raw())),
                        Err(error) => {
                            self.atoms.rollback(checkpoint);
                            return Err(ExecutionError::HeapAllocation(error));
                        }
                    }
                }
                _ => None,
            };
            constant_values.push(value);
        }
        let code = CodeId::from_index(self.loaded_code.len())
            .ok_or(ExecutionError::LoadedModuleLimit { limit: u32::MAX })?;
        self.loaded_code.push(LoadedCode {
            module: module.clone(),
            scope_resolutions: scope_resolutions.into_boxed_slice(),
            constant_values: constant_values.into_boxed_slice(),
        });
        Ok(code)
    }

    /// Executes already-loaded code without repeating module identity or scope-name resolution.
    pub fn execute_loaded(
        &mut self,
        code: CodeId,
        budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        self.execute_loaded_with_batch::<{ tuning::dispatch::DEFAULT_DISPATCH_BATCH }>(code, budget)
    }

    #[inline(always)]
    pub(crate) fn loaded_code(&self, code: CodeId) -> Result<&LoadedCode, ExecutionError> {
        self.loaded_code
            .get(code.index())
            .ok_or(ExecutionError::InvalidCode(code))
    }

    /// Resolves an immutable scope atom once, then retains its stable global slot in loaded code.
    #[inline(always)]
    fn scope_resolution(
        &mut self,
        code: CodeId,
        scope_name: u32,
    ) -> Result<ScopeResolution, ExecutionError> {
        let resolution = self
            .loaded_code(code)?
            .scope_resolutions
            .get(scope_name as usize)
            .copied()
            .ok_or(ExecutionError::InvalidScopeName { code, scope_name })?;
        if resolution.lexical_slot.is_some()
            || resolution.intrinsic_slot.is_some()
            || resolution.global_slot.is_some()
        {
            return Ok(resolution);
        }
        let lexical_slot = self.realm.resolve_lexical(resolution.atom);
        let intrinsic_slot = self.realm.resolve_intrinsic(resolution.atom);
        let global_slot = self.realm.resolve(resolution.atom);
        if lexical_slot.is_none() && intrinsic_slot.is_none() && global_slot.is_none() {
            return Ok(resolution);
        }
        let resolved = ScopeResolution {
            lexical_slot,
            intrinsic_slot,
            global_slot,
            ..resolution
        };
        self.loaded_code
            .get_mut(code.index())
            .expect("validated loaded code remains present")
            .scope_resolutions[scope_name as usize] = resolved;
        Ok(resolved)
    }

    #[inline(always)]
    fn scope_atom(&self, code: CodeId, scope_name: u32) -> Result<AtomId, ExecutionError> {
        self.loaded_code(code)?
            .scope_resolutions
            .get(scope_name as usize)
            .map(|resolution| resolution.atom)
            .ok_or(ExecutionError::InvalidScopeName { code, scope_name })
    }

    /// Converts an ECMAScript primitive into a string or Symbol property-key identity.
    #[cold]
    pub(crate) fn property_key(&mut self, value: Value) -> Result<PropertyKey, ExecutionError> {
        if let Some(raw) = value.as_heap_ref()
            && let Ok(symbol) = self.heap.checked_reference(raw, self.types.symbol)
        {
            let serial = self.heap.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_reference(symbol, self.types.symbol)
                    .map(|symbol| symbol.serial)
                    .map_err(ExecutionError::NoGcBorrow)
            })?;
            return Ok(PropertyKey::Symbol(SymbolId::new(serial, raw)));
        }
        let atom = match value.as_immediate() {
            Some(Immediate::Undefined) => self.intern_intrinsic_name(b"undefined")?,
            Some(Immediate::Null) => self.intern_intrinsic_name(b"null")?,
            Some(Immediate::True) => self.intern_intrinsic_name(b"true")?,
            Some(Immediate::False) => self.intern_intrinsic_name(b"false")?,
            Some(Immediate::Hole | Immediate::Uninitialized) => {
                return Err(ExecutionError::UnsupportedPropertyKey(value));
            }
            None => self.property_key_atom(value)?,
        };
        Ok(PropertyKey::Atom(atom))
    }

    /// Converts supported primitive values to interned PropertyKeys.
    #[cold]
    pub(crate) fn property_key_atom(&mut self, value: Value) -> Result<AtomId, ExecutionError> {
        if let Some(raw) = value.as_heap_ref()
            && let Ok(reference) = self.heap.checked_reference(raw, self.types.string)
        {
            let string = self.heap.with_running_scope(|scope| {
                let root = scope.root(reference).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let string = no_gc
                        .borrow(root, self.types.string)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    match string.as_view() {
                        JsStringView::Latin1(bytes) => JsString::try_from_latin1(bytes),
                        JsStringView::Utf16(code_units) => JsString::try_from_utf16(code_units),
                    }
                    .map_err(ExecutionError::PropertyKeyString)
                })
            })?;
            return self
                .atoms
                .try_intern(string)
                .map_err(ExecutionError::PropertyKeyAtom);
        }
        if let Some(integer) = value.as_i32() {
            let key = Int32PropertyKey::new(integer);
            if let Some(atom) = self.atoms.find_latin1(key.as_bytes()) {
                return Ok(atom);
            }
            let string = JsString::try_from_latin1(key.as_bytes())
                .map_err(ExecutionError::PropertyKeyString)?;
            return self
                .atoms
                .try_intern(string)
                .map_err(ExecutionError::PropertyKeyAtom);
        }

        // Number::toString is only reached after the immediate integer fast path.
        let number = value
            .as_f64()
            .ok_or(ExecutionError::UnsupportedPropertyKey(value))?;
        let mut buffer = ryu_js::Buffer::new();
        let printed = if number == 0.0 {
            "0"
        } else {
            buffer.format(number)
        };
        let string = JsString::try_from_str(printed).map_err(ExecutionError::PropertyKeyString)?;
        self.atoms
            .try_intern(string)
            .map_err(ExecutionError::PropertyKeyAtom)
    }

    /// Executes with a fixed internal batch size so each monomorphization preserves the same fuel contract.
    #[cfg(test)]
    pub(crate) fn execute_with_batch<const N: usize>(
        &mut self,
        module: &CompiledModule,
        budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        let code = self.load_module(module)?;
        self.execute_loaded_with_batch::<N>(code, budget)
    }

    /// Runs one resolved code entry with a test-selectable dispatch batch monomorphization.
    fn execute_loaded_with_batch<const N: usize>(
        &mut self,
        code: CodeId,
        budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        if N == 0 {
            return Err(ExecutionError::InvalidDispatchBatch { batch: N });
        }
        if budget.fuel == u64::MAX && budget.quantum == u32::MAX {
            self.execute_loaded_loop::<N, true>(code, budget)
        } else {
            self.execute_loaded_loop::<N, false>(code, budget)
        }
    }

    /// Selects an exact bounded loop or a compile-time-elided effectively-unbounded loop.
    fn execute_loaded_loop<const N: usize, const UNBOUNDED: bool>(
        &mut self,
        code: CodeId,
        mut budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        let entry_function = self.loaded_code(code)?.module.entry_function();
        self.enter(code, entry_function)?;
        loop {
            if !UNBOUNDED && (budget.fuel == 0 || budget.quantum == 0) {
                return Ok(RunOutcome::BudgetExhausted);
            }
            if let Some(outcome) = self.execute_batch::<N, UNBOUNDED>(&mut budget)? {
                return Ok(outcome);
            }
        }
    }

    /// Executes one fixed-size batch while const-folding budget work only for the MAX/MAX sentinel.
    #[inline(always)]
    fn execute_batch<const N: usize, const UNBOUNDED: bool>(
        &mut self,
        budget: &mut ExecutionBudget,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let (mut code, mut function, mut pc, mut base) = {
            let frame = self
                .fiber
                .frames
                .last()
                .expect("entry frame exists while executing");
            (frame.code, frame.function, frame.pc, frame.base)
        };
        let (mut cursor, mut registers) = self.execution_cursor(code, function, base)?;
        #[cfg(feature = "opcode-profile")]
        self.execution_profile.record_batch_cursor_bind();
        for _ in 0..N {
            if !UNBOUNDED && (budget.fuel == 0 || budget.quantum == 0) {
                #[cfg(feature = "opcode-profile")]
                self.execution_profile.record_budget_flush();
                self.flush_cursor_pc(pc);
                return Ok(Some(RunOutcome::BudgetExhausted));
            }
            let instruction_offset = pc;
            // SAFETY: entry, fallthrough, branches, handlers, and saved return PCs are all verifier-
            // approved instruction starts; every slow exit rebuilds the cursor before resuming.
            let instruction = unsafe { cursor.decode(instruction_offset) };
            let mut next_pc =
                WordOffset::new(instruction_offset.index() + u32::from(instruction.word_len));
            #[cfg(feature = "opcode-profile")]
            let fallthrough_pc = next_pc;
            if !UNBOUNDED {
                budget.fuel -= 1;
                budget.quantum -= 1;
            }
            // SAFETY: `execution_cursor` checked this verified function's complete window, the
            // decoder only returns verifier-approved operands, and no hot operation can resize or
            // expose the register backing before a Slow result invalidates the cursor.
            let hot_control = unsafe {
                execute_verified_hot_instruction(&mut registers, instruction, &mut next_pc)
            };
            #[cfg(feature = "opcode-profile")]
            self.execution_profile
                .record_instruction(instruction.opcode, hot_control == HotControl::Continue);
            if hot_control == HotControl::Continue {
                #[cfg(feature = "opcode-profile")]
                if is_conditional_branch(instruction.opcode) {
                    self.execution_profile
                        .record_branch(instruction.opcode, next_pc != fallthrough_pc);
                }
                pc = next_pc;
                continue;
            }

            #[cfg(feature = "opcode-profile")]
            self.execution_profile.record_slow_flush();
            self.flush_cursor_pc(next_pc);
            let outcome = match self.dispatch(
                code,
                instruction_offset,
                instruction.opcode,
                instruction.operands,
                base,
            ) {
                Ok(outcome) => outcome,
                Err(error) => {
                    let Some(kind) = execution_error_kind(&error) else {
                        #[cfg(feature = "opcode-profile")]
                        self.execution_profile.record_fault_slow_exit();
                        return Err(error);
                    };
                    match self.throw_native_error(kind, instruction_offset) {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            #[cfg(feature = "opcode-profile")]
                            self.execution_profile.record_fault_slow_exit();
                            return Err(error);
                        }
                    }
                }
            };
            if let Some(outcome) = outcome {
                #[cfg(feature = "opcode-profile")]
                self.execution_profile.record_terminal_slow_exit();
                return Ok(Some(outcome));
            }
            #[cfg(feature = "opcode-profile")]
            let previous_activation = (code, function, base);
            (code, function, pc, base) = {
                let frame = self
                    .fiber
                    .frames
                    .last()
                    .expect("continued execution retains an active frame");
                (frame.code, frame.function, frame.pc, frame.base)
            };
            #[cfg(feature = "opcode-profile")]
            if is_conditional_branch(instruction.opcode) {
                self.execution_profile
                    .record_branch(instruction.opcode, pc != fallthrough_pc);
            }
            (cursor, registers) = match self.execution_cursor(code, function, base) {
                Ok(cursor) => cursor,
                Err(error) => {
                    #[cfg(feature = "opcode-profile")]
                    self.execution_profile.record_fault_slow_exit();
                    return Err(error);
                }
            };
            #[cfg(feature = "opcode-profile")]
            self.execution_profile
                .record_slow_rebind(previous_activation != (code, function, base));
        }
        #[cfg(feature = "opcode-profile")]
        self.execution_profile.record_batch_flush();
        self.flush_cursor_pc(pc);
        Ok(None)
    }

    /// Resolves immutable code and checks its register window once per cursor epoch.
    #[inline(always)]
    fn execution_cursor(
        &mut self,
        code: CodeId,
        function: FunctionId,
        base: u32,
    ) -> Result<(BytecodeCursor, RegisterWindow), ExecutionError> {
        let (bytecode, register_count) = {
            let function = self
                .loaded_code(code)?
                .module
                .function(function)
                .ok_or(ExecutionError::MissingEntryFunction(function))?;
            (
                // SAFETY: append-only LoadedCode owns this CompiledModule and its immutable Arc
                // function backing for the isolate lifetime; the cursor never leaves execution.
                unsafe { BytecodeCursor::new(function.bytecode()) },
                function.layout().register_count,
            )
        };
        let registers = RegisterWindow::new(
            &mut self.fiber.registers,
            base as usize,
            register_count as usize,
        )
        .ok_or(ExecutionError::RegisterWindowTooLarge(register_count))?;
        Ok((bytecode, registers))
    }

    /// Publishes the local cursor before any slow operation can observe or mutate the active fiber.
    #[inline(always)]
    pub(crate) fn flush_cursor_pc(&mut self, pc: WordOffset) {
        self.fiber
            .frames
            .last_mut()
            .expect("cursor flush retains an active frame")
            .pc = pc;
    }

    /// Implements one verified opcode without conflating engine faults with language exceptions.
    #[inline(never)]
    pub(crate) fn dispatch(
        &mut self,
        code: CodeId,
        instruction_offset: WordOffset,
        opcode: Opcode,
        operands: [u32; 3],
        base: u32,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match opcode {
            Opcode::Nop => {}
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
                let constant_index = operands[1] as usize;
                let loaded = self.loaded_code(code)?;
                let constant = loaded
                    .module
                    .constants()
                    .get(constant_index)
                    .ok_or(ExecutionError::UnsupportedConstant(operands[1]))?;
                let value = match constant {
                    BytecodeConstant::NumberBits(bits) => Value::from_f64(f64::from_bits(*bits)),
                    BytecodeConstant::String(_) => loaded
                        .constant_values
                        .get(constant_index)
                        .copied()
                        .flatten()
                        .ok_or(ExecutionError::UnsupportedConstant(operands[1]))?,
                    _ => return Err(ExecutionError::UnsupportedConstant(operands[1])),
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::Move => {
                let value = self.read(base, operands[1])?;
                self.write(base, operands[0], value)?;
            }
            Opcode::Not => {
                let value = if self.is_truthy_value(self.read(base, operands[1])?)? {
                    Value::from_immediate(Immediate::False)
                } else {
                    Value::from_immediate(Immediate::True)
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::Negate => {
                let input = self.read(base, operands[1])?;
                if self.is_object_value(input) {
                    self.dispatch_object_primitive_conversion(
                        ConversionConsumer::Negate,
                        base,
                        operands[0],
                        Value::from_immediate(Immediate::Undefined),
                        input,
                        instruction_offset,
                    )?;
                } else {
                    let value = numeric_negate(self.convert_to_number(input)?);
                    self.write(base, operands[0], value)?;
                }
            }
            Opcode::ToNumber => {
                let input = self.read(base, operands[1])?;
                if self.is_object_value(input) {
                    self.dispatch_object_primitive_conversion(
                        ConversionConsumer::ToNumber,
                        base,
                        operands[0],
                        Value::from_immediate(Immediate::Undefined),
                        input,
                        instruction_offset,
                    )?;
                } else {
                    let value = self.convert_to_number(input)?;
                    self.write(base, operands[0], value)?;
                }
            }
            Opcode::BitwiseNot => {
                let input = self.read(base, operands[1])?;
                if self.is_object_value(input) {
                    self.dispatch_object_primitive_conversion(
                        ConversionConsumer::BitwiseNot,
                        base,
                        operands[0],
                        Value::from_immediate(Immediate::Undefined),
                        input,
                        instruction_offset,
                    )?;
                } else {
                    let number = self.convert_to_number(input)?;
                    self.write(base, operands[0], numeric_bitwise_not(number))?;
                }
            }
            Opcode::Add => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                if numeric_value(left).is_some() && numeric_value(right).is_some() {
                    self.write(base, operands[0], numeric_binary(opcode, left, right))?;
                } else if self.is_object_value(left) {
                    self.dispatch_object_primitive_conversion(
                        ConversionConsumer::AddLeft,
                        base,
                        operands[0],
                        right,
                        left,
                        instruction_offset,
                    )?;
                } else if self.is_object_value(right) {
                    self.dispatch_object_primitive_conversion(
                        ConversionConsumer::AddRight,
                        base,
                        operands[0],
                        left,
                        right,
                        instruction_offset,
                    )?;
                } else {
                    let result = self.add_primitive_values(left, right)?;
                    self.write(base, operands[0], result)?;
                }
            }
            Opcode::Sub
            | Opcode::Mul
            | Opcode::Div
            | Opcode::BitwiseAnd
            | Opcode::BitwiseOr
            | Opcode::BitwiseXor
            | Opcode::ShiftLeft
            | Opcode::ShiftRight
            | Opcode::ShiftRightUnsigned
            | Opcode::Remainder
            | Opcode::Exponentiate => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                if self.is_object_value(left) {
                    self.dispatch_object_primitive_conversion(
                        ConversionConsumer::BinaryLeft(opcode),
                        base,
                        operands[0],
                        right,
                        left,
                        instruction_offset,
                    )?;
                } else {
                    let left = self.convert_to_number(left)?;
                    if self.is_object_value(right) {
                        self.dispatch_object_primitive_conversion(
                            ConversionConsumer::BinaryRight(opcode),
                            base,
                            operands[0],
                            left,
                            right,
                            instruction_offset,
                        )?;
                    } else {
                        let right = self.convert_to_number(right)?;
                        self.write(
                            base,
                            operands[0],
                            numeric_binary_operation(opcode, left, right),
                        )?;
                    }
                }
            }
            Opcode::LessThan | Opcode::GreaterThan | Opcode::LessEqual | Opcode::GreaterEqual => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                if numeric_value(left).is_some() && numeric_value(right).is_some() {
                    self.write(base, operands[0], numeric_relational(opcode, left, right))?;
                } else if self.is_object_value(left) {
                    self.dispatch_object_primitive_conversion(
                        ConversionConsumer::RelationalLeft(opcode),
                        base,
                        operands[0],
                        right,
                        left,
                        instruction_offset,
                    )?;
                } else if self.is_object_value(right) {
                    self.dispatch_object_primitive_conversion(
                        ConversionConsumer::RelationalRight(opcode),
                        base,
                        operands[0],
                        left,
                        right,
                        instruction_offset,
                    )?;
                } else {
                    let result = self.relational_primitive_values(opcode, left, right)?;
                    self.write(base, operands[0], result)?;
                }
            }
            Opcode::StrictEqual => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                let value = if self.strict_equal_values(left, right)? {
                    Value::from_immediate(Immediate::True)
                } else {
                    Value::from_immediate(Immediate::False)
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::LooseEqual | Opcode::LooseNotEqual => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                if self.is_object_value(left) || self.is_object_value(right) {
                    self.dispatch_object_loose_equality(
                        opcode,
                        base,
                        operands[0],
                        left,
                        right,
                        instruction_offset,
                    )?;
                    return Ok(None);
                }
                let equal = self.loose_equal_values(left, right)?;
                let result = if opcode == Opcode::LooseEqual {
                    equal
                } else {
                    !equal
                };
                self.write(
                    base,
                    operands[0],
                    Value::from_immediate(if result {
                        Immediate::True
                    } else {
                        Immediate::False
                    }),
                )?;
            }
            Opcode::HasProperty => {
                let key = self.property_key(self.read(base, operands[1])?)?;
                let receiver = self.read(base, operands[2])?;
                let result = !matches!(
                    self.resolve_property_read(receiver, key)?,
                    PropertyRead::Missing
                );
                self.write(
                    base,
                    operands[0],
                    Value::from_immediate(if result {
                        Immediate::True
                    } else {
                        Immediate::False
                    }),
                )?;
            }
            Opcode::ToPropertyKey | Opcode::ToPropertyKeyForIn => {
                self.dispatch_to_property_key(
                    base,
                    operands[0],
                    operands[1],
                    operands[2],
                    opcode == Opcode::ToPropertyKeyForIn,
                    instruction_offset,
                )?;
                return Ok(None);
            }
            Opcode::TypeofScope => {
                let resolution = self.scope_resolution(code, operands[1])?;
                let value = self
                    .scope_value(resolution)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined));
                let value = self.typeof_value(value)?;
                self.write(base, operands[0], value)?;
            }
            Opcode::DeleteById => {
                let receiver = self.read(base, operands[1])?;
                let key = self.scope_atom(code, operands[2])?;
                let result = self.delete_data_property_from_bytecode(receiver, key)?;
                self.write(
                    base,
                    operands[0],
                    Value::from_immediate(if result {
                        Immediate::True
                    } else {
                        Immediate::False
                    }),
                )?;
            }
            Opcode::DeleteByValue => {
                let receiver = self.read(base, operands[1])?;
                let key = self.property_key(self.read(base, operands[2])?)?;
                let result = self.delete_data_property_from_bytecode(receiver, key)?;
                self.write(
                    base,
                    operands[0],
                    Value::from_immediate(if result {
                        Immediate::True
                    } else {
                        Immediate::False
                    }),
                )?;
            }
            Opcode::Typeof => {
                let value = self.typeof_value(self.read(base, operands[1])?)?;
                self.write(base, operands[0], value)?;
            }
            Opcode::InstanceOf => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                let value = if self.ordinary_instance_of(left, right)? {
                    Value::from_immediate(Immediate::True)
                } else {
                    Value::from_immediate(Immediate::False)
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::Jump => self.set_pc(WordOffset::new(operands[0])),
            Opcode::JumpIfFalse => {
                if !self.is_truthy_value(self.read(base, operands[0])?)? {
                    self.set_pc(WordOffset::new(operands[1]));
                }
            }
            Opcode::JumpIfTrue => {
                if self.is_truthy_value(self.read(base, operands[0])?)? {
                    self.set_pc(WordOffset::new(operands[1]));
                }
            }
            Opcode::JumpIfNotNullish => {
                if !is_nullish(self.read(base, operands[0])?) {
                    self.set_pc(WordOffset::new(operands[1]));
                }
            }
            Opcode::LoadScope => {
                let resolution = self.scope_resolution(code, operands[1])?;
                let value = self
                    .scope_value(resolution)?
                    .ok_or(ExecutionError::UnresolvedBinding(resolution.atom))?;
                self.write(base, operands[0], value)?;
            }
            Opcode::StoreScope => {
                let value = self.read(base, operands[0])?;
                self.store_scope(code, operands[1], value)?;
            }
            Opcode::StoreResolvedScope => {
                let value = self.read(base, operands[0])?;
                self.store_resolved_scope(code, operands[1], value)?;
            }
            Opcode::LoadEnvironment => {
                let value = self.load_environment(operands[1], operands[2])?;
                self.write(base, operands[0], value)?;
            }
            Opcode::StoreEnvironment => {
                let value = self.read(base, operands[0])?;
                self.store_environment(operands[1], operands[2], value)?;
            }
            Opcode::DeclareScope => {
                self.declare_scope(code, operands[0])?;
            }
            Opcode::DeclareGlobalLexical => {
                self.declare_global_lexical(code, operands[0], operands[1] != 0)?;
            }
            Opcode::InitializeGlobalLexical => {
                let value = self.read(base, operands[0])?;
                self.initialize_global_lexical(code, operands[1], value)?;
            }
            Opcode::CreateClosure => {
                self.create_closure(code, base, operands[0], FunctionId::new(operands[1]))?
            }
            Opcode::CreateObject => {
                let object = self.create_ordinary_object()?;
                self.write(base, operands[0], object)?;
            }
            Opcode::CreateArray => {
                let prototype = self
                    .realm
                    .array_prototype
                    .expect("Array prototype initializes before array literals");
                let object = self.create_array_object_with_prototype(prototype)?;
                self.write(base, operands[0], object)?;
            }
            Opcode::CreateForInIterator => {
                let source = self.read(base, operands[1])?;
                let iterator = self.create_for_in_iterator(source)?;
                self.write(base, operands[0], iterator)?;
            }
            Opcode::ForInNext => {
                let iterator = self.read(base, operands[1])?;
                let value = self.for_in_next(iterator)?;
                self.write(base, operands[0], value)?;
            }
            Opcode::LoadException => {
                let value = self
                    .fiber
                    .pending_exception
                    .take()
                    .ok_or(ExecutionError::MissingPendingException)?;
                self.write(base, operands[0], value)?;
            }
            Opcode::LoadThis => {
                let value = self
                    .fiber
                    .frames
                    .last()
                    .expect("this load always has an active frame")
                    .this_value;
                self.write(base, operands[0], value)?;
            }
            Opcode::LoadNewTarget => {
                let value = self
                    .fiber
                    .frames
                    .last()
                    .expect("new.target load always has an active frame")
                    .new_target;
                self.write(base, operands[0], value)?;
            }
            Opcode::LoadArgumentsLength => {
                let length = self
                    .fiber
                    .frames
                    .last()
                    .expect("arguments length load always has an active frame")
                    .argument_count;
                self.write(base, operands[0], safe_integer_value(u64::from(length)))?;
            }
            Opcode::GetById => {
                let receiver = self.read(base, operands[1])?;
                let key = self.scope_atom(code, operands[2])?;
                return self.dispatch_property_read(
                    base,
                    operands[0],
                    receiver,
                    key.into(),
                    instruction_offset,
                );
            }
            Opcode::SetById => {
                let receiver = self.read(base, operands[0])?;
                let value = self.read(base, operands[1])?;
                let key = self.scope_atom(code, operands[2])?;
                return self.dispatch_property_write(
                    base,
                    operands[1],
                    receiver,
                    key.into(),
                    value,
                    instruction_offset,
                );
            }
            Opcode::GetByValue => {
                let receiver = self.read(base, operands[1])?;
                let key = self.property_key(self.read(base, operands[2])?)?;
                return self.dispatch_property_read(
                    base,
                    operands[0],
                    receiver,
                    key,
                    instruction_offset,
                );
            }
            Opcode::SetByValue => {
                let receiver = self.read(base, operands[0])?;
                let value = self.read(base, operands[1])?;
                let key = self.property_key(self.read(base, operands[2])?)?;
                return self.dispatch_property_write(
                    base,
                    operands[1],
                    receiver,
                    key,
                    value,
                    instruction_offset,
                );
            }
            Opcode::Call => {
                let callee = self.read(base, operands[1])?;
                self.call(CallSite {
                    caller_base: base,
                    destination: operands[0],
                    callee,
                    argument_base: base
                        .checked_add(operands[1])
                        .and_then(|base| base.checked_add(1))
                        .ok_or(ExecutionError::RegisterWindowTooLarge(operands[2]))?,
                    argument_prefix: None,
                    argument_prefix_offset: 0,
                    argument_prefix_count: 0,
                    argument_count: operands[2],
                    this_value: Value::from_immediate(Immediate::Undefined),
                    new_target: Value::from_immediate(Immediate::Undefined),
                    construct_receiver: None,
                    call_site: instruction_offset,
                })?;
            }
            Opcode::CallWithReceiver => {
                let receiver = self.read(base, operands[1])?;
                let callee = self.read(base, operands[1] + 1)?;
                self.call(CallSite {
                    caller_base: base,
                    destination: operands[0],
                    callee,
                    argument_base: base
                        .checked_add(operands[1])
                        .and_then(|base| base.checked_add(2))
                        .ok_or(ExecutionError::RegisterWindowTooLarge(operands[2]))?,
                    argument_prefix: None,
                    argument_prefix_offset: 0,
                    argument_prefix_count: 0,
                    argument_count: operands[2],
                    this_value: receiver,
                    new_target: Value::from_immediate(Immediate::Undefined),
                    construct_receiver: None,
                    call_site: instruction_offset,
                })?;
            }
            Opcode::Construct => self.construct(
                base,
                operands[0],
                operands[1],
                operands[2],
                instruction_offset,
            )?,
            Opcode::Return => {
                let value = self.read(base, operands[0])?;
                if !self
                    .fiber
                    .frames
                    .last()
                    .expect("return retains its frame")
                    .has_finally
                {
                    return self.finish_return(value);
                }
                return self
                    .dispatch_abrupt(CompletionRecord::return_value(value), instruction_offset);
            }
            Opcode::ReturnUndefined => {
                let value = Value::from_immediate(Immediate::Undefined);
                if !self
                    .fiber
                    .frames
                    .last()
                    .expect("return retains its frame")
                    .has_finally
                {
                    return self.finish_return(value);
                }
                return self
                    .dispatch_abrupt(CompletionRecord::return_value(value), instruction_offset);
            }
            Opcode::Throw => {
                let value = self.read(base, operands[0])?;
                return self.throw_value(value, instruction_offset);
            }
            Opcode::EnterFinally => {
                let (index, handler) = self
                    .find_covering_finally(instruction_offset)?
                    .ok_or(ExecutionError::MissingCompletionRecord)?;
                self.enter_finalizer(index, handler, CompletionRecord::normal(None))?;
            }
            Opcode::ResumeCompletion => {
                return self.resume_completion(instruction_offset);
            }
            Opcode::BreakThroughFinally => {
                return self.dispatch_abrupt(
                    CompletionRecord::break_target(None, WordOffset::new(operands[0])),
                    instruction_offset,
                );
            }
            Opcode::ContinueThroughFinally => {
                return self.dispatch_abrupt(
                    CompletionRecord::continue_target(None, WordOffset::new(operands[0])),
                    instruction_offset,
                );
            }
            _ => return Err(ExecutionError::UnsupportedOpcode(opcode)),
        }
        Ok(None)
    }

    /// Completes a data read immediately or suspends on one getter callback frame.
    fn dispatch_property_read(
        &mut self,
        caller_base: u32,
        destination: u32,
        receiver: Value,
        key: PropertyKey,
        call_site: WordOffset,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match self.resolve_property_read(receiver, key)? {
            PropertyRead::Missing => {
                self.write(
                    caller_base,
                    destination,
                    Value::from_immediate(Immediate::Undefined),
                )?;
                Ok(None)
            }
            PropertyRead::Data(value) => {
                self.write(caller_base, destination, value)?;
                Ok(None)
            }
            PropertyRead::Accessor(getter)
                if getter.as_immediate() == Some(Immediate::Undefined) =>
            {
                self.write(
                    caller_base,
                    destination,
                    Value::from_immediate(Immediate::Undefined),
                )?;
                Ok(None)
            }
            PropertyRead::Accessor(callee) => self.dispatch_property_callback(
                NativeContinuation::property_get(
                    NativeContinuationSite {
                        caller_base,
                        destination,
                        call_site,
                    },
                    PropertyCallbackMode::Ordinary,
                    receiver,
                ),
                callee,
            ),
        }
    }

    /// Applies assignment rejection at the strict boundary or suspends on one setter callback.
    fn dispatch_property_write(
        &mut self,
        caller_base: u32,
        value_register: u32,
        receiver: Value,
        key: PropertyKey,
        value: Value,
        call_site: WordOffset,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match self.resolve_property_write(receiver, key, value)? {
            PropertyWrite::Complete(success) => {
                self.finish_property_write(receiver, success)?;
                Ok(None)
            }
            PropertyWrite::Setter(callee) => self.dispatch_property_callback(
                NativeContinuation::property_set(
                    NativeContinuationSite {
                        caller_base,
                        destination: value_register,
                        call_site,
                    },
                    receiver,
                    value,
                ),
                callee,
            ),
        }
    }

    /// Publishes callback state before using the existing iterative call/frame machinery.
    pub(crate) fn dispatch_property_callback(
        &mut self,
        continuation: NativeContinuation,
        callee: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let site = continuation.site();
        let (receiver, argument_base, argument_count) = match continuation.kind() {
            NativeContinuationKind::PropertyGet(mode) => {
                let receiver = continuation.first();
                let receiver = if mode == PropertyCallbackMode::Descriptor {
                    let state = self.pending_property_descriptor_reference(receiver)?;
                    self.pending_property_descriptor_source(state)?
                } else if matches!(
                    mode,
                    PropertyCallbackMode::ArrayIteratorLength
                        | PropertyCallbackMode::ArrayIteratorElement
                ) {
                    continuation.second()
                } else {
                    receiver
                };
                (receiver, 0, 0)
            }
            NativeContinuationKind::PropertySet => (
                continuation.first(),
                site.caller_base
                    .checked_add(site.destination)
                    .ok_or(ExecutionError::RegisterWindowTooLarge(1))?,
                1,
            ),
            NativeContinuationKind::Conversion { .. } => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ConversionCallRoot => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
        };
        // The continuation omits `callee` to stay 32 bytes: before frame publication it remains
        // reachable through the receiver's accessor pair (or descriptor state -> source chain).
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(|error| match error {
                CompletionStackError::Limit { limit, requested } => {
                    ExecutionError::CompletionStackLimit { limit, requested }
                }
                CompletionStackError::AllocationFailed => {
                    ExecutionError::CompletionAllocationFailed
                }
            })?;
        let frame_depth = self.fiber.frames.len();
        let call_result = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee,
            argument_base,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count,
            this_value: receiver,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        });
        if let Err(error) = call_result {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("a suspended accessor callback publishes its callee frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(None);
        }
        let continuation = self.pop_native_continuation()?;
        let returned = self.read(site.caller_base, site.destination)?;
        self.resume_native_continuation(continuation, returned)
    }

    /// Calls one descriptor-field getter while keeping synchronous errors at the bytecode boundary.
    pub(crate) fn call_property_descriptor_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingPropertyDescriptor>,
        callee: Value,
    ) -> Result<(), ExecutionError> {
        let receiver = self.pending_property_descriptor_source(state)?;
        let continuation = NativeContinuation::property_get(
            site,
            PropertyCallbackMode::Descriptor,
            Value::from_heap_ref(state.raw()),
        );
        // `state` traces the descriptor source whose accessor pair retains `callee` until call.
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(|error| match error {
                CompletionStackError::Limit { limit, requested } => {
                    ExecutionError::CompletionStackLimit { limit, requested }
                }
                CompletionStackError::AllocationFailed => {
                    ExecutionError::CompletionAllocationFailed
                }
            })?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee,
            argument_base: 0,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 0,
            this_value: receiver,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        }) {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("a suspended descriptor getter publishes its callee frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let returned = self.read(site.caller_base, site.destination)?;
        let continuation = self.pop_native_continuation()?;
        if continuation.kind()
            != NativeContinuationKind::PropertyGet(PropertyCallbackMode::Descriptor)
        {
            return Err(ExecutionError::MissingNativeContinuation);
        }
        let receiver = continuation.first();
        let state = self.pending_property_descriptor_reference(receiver)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.resume_property_descriptor(site, state, returned)
    }

    /// Converts one ordinary [[Set]] false result into strict-mode TypeError semantics.
    #[inline]
    fn finish_property_write(&self, receiver: Value, success: bool) -> Result<(), ExecutionError> {
        let strictness = self
            .fiber
            .frames
            .last()
            .expect("property assignment always has an active frame")
            .strictness;
        if !success && strictness == FunctionStrictness::Strict {
            return Err(ExecutionError::ReadOnlyProperty(receiver));
        }
        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn throw_native_error(
        &mut self,
        kind: NativeErrorKind,
        instruction_offset: WordOffset,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let error = self.create_native_error(kind, None)?;
        self.throw_value(error, instruction_offset)
    }

    #[inline(always)]
    pub(crate) fn scope_value(
        &self,
        resolution: ScopeResolution,
    ) -> Result<Option<Value>, ExecutionError> {
        if let Some(slot) = resolution.lexical_slot {
            return self.realm.lexical_value(slot).map(Some);
        }
        if let Some(slot) = resolution.intrinsic_slot {
            return Ok(Some(self.realm.intrinsic_value(slot)));
        }
        Ok(resolution
            .global_slot
            .and_then(|slot| self.realm.get_slot(slot)))
    }

    /// Writes through a cached global slot or publishes the binding once on the cold path.
    #[inline(always)]
    fn store_scope(
        &mut self,
        code: CodeId,
        scope_name: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let resolution = self.scope_resolution(code, scope_name)?;
        if let Some(slot) = resolution.intrinsic_slot {
            return self.realm.set_intrinsic(slot, value);
        }
        if let Some(slot) = resolution.global_slot {
            self.realm.set_slot(slot, value);
            return Ok(());
        }
        self.realm.set(resolution.atom, value)
    }

    #[inline(always)]
    fn declare_scope(&mut self, code: CodeId, scope_name: u32) -> Result<(), ExecutionError> {
        let resolution = self.scope_resolution(code, scope_name)?;
        self.declare_scope_resolution(resolution)
    }

    #[inline(always)]
    pub(crate) fn declare_scope_resolution(
        &mut self,
        resolution: ScopeResolution,
    ) -> Result<(), ExecutionError> {
        if resolution.lexical_slot.is_some() {
            return Err(ExecutionError::GlobalLexicalRedeclaration(resolution.atom));
        }
        if self.scope_value(resolution)?.is_some() {
            return Ok(());
        }
        self.realm
            .set(resolution.atom, Value::from_immediate(Immediate::Undefined))
    }

    /// Updates a mutable global or applies the strict/sloppy primitive-intrinsic write contract.
    fn store_resolved_scope(
        &mut self,
        code: CodeId,
        scope_name: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let resolution = self.scope_resolution(code, scope_name)?;
        if let Some(slot) = resolution.lexical_slot {
            return self.realm.set_lexical(slot, value);
        }
        if let Some(slot) = resolution.intrinsic_slot {
            let strict = self
                .fiber
                .frames
                .last()
                .is_some_and(|frame| frame.strictness == FunctionStrictness::Strict);
            return match self.realm.set_intrinsic(slot, value) {
                Err(ExecutionError::ReadOnlyBinding(_)) if !strict => Ok(()),
                result => result,
            };
        }
        if let Some(slot) = resolution.global_slot {
            self.realm.set_slot(slot, value);
            return Ok(());
        }
        let strict = self
            .fiber
            .frames
            .last()
            .is_some_and(|frame| frame.strictness == FunctionStrictness::Strict);
        if strict {
            Err(ExecutionError::UnresolvedBinding(resolution.atom))
        } else {
            self.realm.set(resolution.atom, value)
        }
    }

    fn declare_global_lexical(
        &mut self,
        code: CodeId,
        scope_name: u32,
        mutable: bool,
    ) -> Result<(), ExecutionError> {
        let resolution = self.scope_resolution(code, scope_name)?;
        if resolution.lexical_slot.is_some()
            || resolution.intrinsic_slot.is_some()
            || resolution.global_slot.is_some()
        {
            return Err(ExecutionError::GlobalLexicalRedeclaration(resolution.atom));
        }
        self.realm.declare_lexical(resolution.atom, mutable)
    }

    fn initialize_global_lexical(
        &mut self,
        code: CodeId,
        scope_name: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let resolution = self.scope_resolution(code, scope_name)?;
        let slot = resolution
            .lexical_slot
            .ok_or(ExecutionError::UnresolvedBinding(resolution.atom))?;
        self.realm.initialize_lexical(slot, value)
    }

    fn environment_at_depth(&mut self, depth: u32) -> Result<GcRef<Environment>, ExecutionError> {
        let mut environment = self
            .fiber
            .frames
            .last()
            .and_then(|frame| frame.environment)
            .ok_or(ExecutionError::MissingEnvironment)?;
        for _ in 0..depth {
            environment = self.heap.with_running_scope(|scope| {
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_reference(environment, self.types.environment)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .parent()
                        .ok_or(ExecutionError::MissingEnvironment)
                })
            })?;
        }
        Ok(environment)
    }

    fn load_environment(&mut self, depth: u32, slot: u32) -> Result<Value, ExecutionError> {
        let environment = self.environment_at_depth(depth)?;
        self.heap.with_running_scope(|scope| {
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_reference(environment, self.types.environment)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .load(slot)
                    .map_err(|error| environment_access_error(depth, slot, error))
            })
        })
    }

    /// Mutates one environment slot and records an old-to-young edge when the value is managed.
    fn store_environment(
        &mut self,
        depth: u32,
        slot: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let environment = self.environment_at_depth(depth)?;
        self.heap.with_running_scope(|scope| {
            scope.with_no_gc_scope(|no_gc| {
                let environment = no_gc
                    .borrow_reference_mut(environment, self.types.environment)
                    .map_err(ExecutionError::NoGcBorrow)?;
                environment
                    .store(slot, value)
                    .map_err(|error| environment_access_error(depth, slot, error))
            })
        })?;
        if let Some(target) = value.as_heap_ref() {
            self.heap
                .write_barrier(environment.raw(), target)
                .map_err(ExecutionError::HeapReference)?;
        }
        Ok(())
    }

    /// Allocates non-empty captured-slot backing after the current activation frame is rooted.
    fn allocate_current_environment(
        &mut self,
        kind: FunctionKind,
        slot_count: NonZeroU32,
    ) -> Result<(), ExecutionError> {
        let parent = self.fiber.frames.last().and_then(|frame| frame.environment);
        let environment_kind = EnvironmentKind::for_activation(kind, parent.is_some());
        let mut environment = if kind == FunctionKind::Module {
            Environment::try_bindings(environment_kind, parent, slot_count, |_| {
                BindingState::new(true, false)
            })
        } else {
            Environment::try_captured(environment_kind, parent, slot_count)
        }
        .map_err(|_| ExecutionError::EnvironmentStorageAllocationFailed)?;
        if kind == FunctionKind::Module {
            for slot in 0..slot_count.get() {
                environment
                    .initialize(slot, Value::from_immediate(Immediate::Undefined))
                    .expect("fresh module binding slots initialize exactly once");
            }
        }
        debug_assert_eq!(environment.kind(), environment_kind);
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let environment = self
            .heap
            .try_allocate_external_with_gc(
                self.types.environment,
                0,
                environment,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        self.fiber
            .frames
            .last_mut()
            .expect("environment allocation retains its frame")
            .environment = Some(environment);
        Ok(())
    }

    pub(crate) fn enter(
        &mut self,
        code: CodeId,
        function_id: FunctionId,
    ) -> Result<(), ExecutionError> {
        let (layout, kind, strictness) = {
            let function = self
                .loaded_code(code)?
                .module
                .function(function_id)
                .ok_or(ExecutionError::MissingEntryFunction(function_id))?;
            (function.layout(), function.kind(), function.strictness())
        };
        let register_count = usize::try_from(layout.register_count)
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(layout.register_count))?;
        if layout.register_count > self.stack_limits.max_registers {
            return Err(ExecutionError::RegisterStackLimit {
                limit: self.stack_limits.max_registers,
                requested: layout.register_count,
            });
        }
        if self.stack_limits.max_frames == 0 {
            return Err(ExecutionError::CallStackLimit { limit: 0 });
        }
        self.fiber.frames.clear();
        self.fiber.registers.clear();
        self.fiber.handlers.clear();
        self.fiber.completions.clear();
        self.fiber
            .completions
            .set_limit(self.stack_limits.max_completions);
        self.fiber.pending_exception = None;
        self.reserve_entry_state(layout, register_count)?;
        self.fiber
            .registers
            .resize(register_count, Value::from_immediate(Immediate::Undefined));
        self.fiber.frames.push(Frame {
            code,
            function: function_id,
            pc: WordOffset::new(0),
            base: 0,
            environment: None,
            return_register: None,
            return_continuation: false,
            this_value: if matches!(kind, FunctionKind::Module) {
                Value::from_immediate(Immediate::Undefined)
            } else {
                self.realm
                    .global_object
                    .expect("realm initialization publishes a global object")
            },
            new_target: Value::from_immediate(Immediate::Undefined),
            strictness,
            has_finally: layout.max_completion_depth != 0,
            argument_base: 0,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 0,
            handler_base: 0,
            completion_base: 0,
            construct_receiver: None,
            call_site: None,
        });
        let Some(slot_count) = NonZeroU32::new(layout.environment_slot_count) else {
            return Ok(());
        };
        self.allocate_current_environment(kind, slot_count)
    }

    /// Allocates a real GC-managed callable instead of encoding FunctionId in a reserved Value tag.
    #[inline(never)]
    fn create_closure(
        &mut self,
        code: CodeId,
        base: u32,
        destination: u32,
        function: FunctionId,
    ) -> Result<(), ExecutionError> {
        self.loaded_code(code)?
            .module
            .function(function)
            .ok_or(ExecutionError::MissingEntryFunction(function))?;
        let environment = self.fiber.frames.last().and_then(|frame| frame.environment);
        let internal_prototype = self
            .realm
            .function_prototype
            .expect("function intrinsics initialize before bytecode execution");
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let closure = self
            .heap
            .try_allocate_with_gc(
                self.types.function,
                0,
                0,
                FunctionObject {
                    executable: FunctionExecutable::Bytecode {
                        code,
                        function,
                        environment,
                    },
                    function_prototype: None,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: internal_prototype,
                    },
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        self.write(base, destination, Value::from_heap_ref(closure.raw()))
    }

    /// Validates the constructor before allocation, creates its receiver, and pushes one JS frame.
    #[inline(never)]
    fn construct(
        &mut self,
        caller_base: u32,
        destination: u32,
        callee_register: u32,
        argument_count: u32,
        call_site: WordOffset,
    ) -> Result<(), ExecutionError> {
        let constructor = self.read(caller_base, callee_register)?;
        let mut site = CallSite {
            caller_base,
            destination,
            callee: constructor,
            argument_base: caller_base
                .checked_add(callee_register)
                .and_then(|base| base.checked_add(1))
                .ok_or(ExecutionError::RegisterWindowTooLarge(argument_count))?,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: constructor,
            construct_receiver: None,
            call_site,
        };
        loop {
            let callable = self
                .resolve_function_object(site.callee)
                .map_err(|_| ExecutionError::NonConstructor(site.callee))?;
            match callable.executable {
                FunctionExecutable::Bound(data) => {
                    if site.argument_prefix.is_some() {
                        return Err(ExecutionError::BoundArgumentCountOverflow);
                    }
                    let bound = self.bound_function_snapshot(data)?;
                    site.argument_count = site
                        .argument_count
                        .checked_add(bound.argument_count)
                        .ok_or(ExecutionError::BoundArgumentCountOverflow)?;
                    site.argument_prefix = Some(data);
                    site.argument_prefix_count = bound.argument_count;
                    let (target, new_target) =
                        self.resolve_bound_construct_target(site.callee, site.new_target)?;
                    debug_assert_eq!(target, bound.call_target);
                    site.callee = target;
                    site.new_target = new_target;
                }
                FunctionExecutable::Native(NativeFunction::NumberConstructor) => {
                    return self.dispatch_conversion_native(
                        NativeFunction::NumberConstructor,
                        &site,
                        true,
                    );
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::StringConstructor
                    | NativeFunction::BooleanConstructor),
                ) => {
                    let value = self.primitive_constructor_value(native, &site)?;
                    return self.write(caller_base, destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ObjectConstructor) => {
                    let object = self.create_object_from_site(&site)?;
                    return self.write(caller_base, destination, object);
                }
                FunctionExecutable::Native(NativeFunction::ErrorConstructor(kind)) => {
                    let message = self.call_argument(&site, 0)?;
                    let error = self.create_native_error(kind, message)?;
                    return self.write(caller_base, destination, error);
                }
                FunctionExecutable::Native(NativeFunction::ArrayConstructor) => {
                    let array = self.create_array_from_site(&site)?;
                    return self.write(caller_base, destination, array);
                }
                FunctionExecutable::Native(NativeFunction::FunctionConstructor) => {
                    return Err(ExecutionError::UnsupportedDynamicFunctionConstructor);
                }
                FunctionExecutable::Bytecode { .. } => break,
                FunctionExecutable::Native(_) => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
            }
        }
        let prototype_atom = self.prototype_atom()?;
        let prototype = self
            .get_data_property(site.new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .unwrap_or(Value::from_immediate(Immediate::Null));
        let receiver = self.create_ordinary_object_with_prototype(prototype)?;
        site.this_value = receiver;
        site.construct_receiver = Some(receiver);
        self.call(site)
    }

    /// Applies each bound exotic's observable newTarget substitution without merging arguments.
    pub(crate) fn resolve_bound_construct_target(
        &mut self,
        mut target: Value,
        mut new_target: Value,
    ) -> Result<(Value, Value), ExecutionError> {
        loop {
            let function = self.resolve_function_object(target)?;
            let FunctionExecutable::Bound(data) = function.executable else {
                return Ok((target, new_target));
            };
            let bound = self.bound_function_snapshot(data)?;
            if new_target == target {
                new_target = bound.bound_target;
            }
            target = bound.bound_target;
        }
    }

    /// Resolves native forwarding iteratively, then pushes one exact bytecode frame when required.
    #[inline(never)]
    pub(crate) fn call(&mut self, mut site: CallSite) -> Result<(), ExecutionError> {
        loop {
            match self.resolve_function_executable(site.callee)? {
                FunctionExecutable::Bound(data) => {
                    if site.argument_prefix.is_some() {
                        return Err(ExecutionError::BoundArgumentCountOverflow);
                    }
                    let bound = self.bound_function_snapshot(data)?;
                    site.argument_count = site
                        .argument_count
                        .checked_add(bound.argument_count)
                        .ok_or(ExecutionError::BoundArgumentCountOverflow)?;
                    site.argument_prefix = Some(data);
                    site.argument_prefix_count = bound.argument_count;
                    site.callee = bound.call_target;
                    site.this_value = bound.bound_this;
                }
                FunctionExecutable::Bytecode {
                    code,
                    function,
                    environment,
                } => {
                    let (kind, layout, strictness) = {
                        let function_template =
                            self.loaded_code(code)?
                                .module
                                .function(function)
                                .ok_or(ExecutionError::MissingEntryFunction(function))?;
                        (
                            function_template.kind(),
                            function_template.layout(),
                            function_template.strictness(),
                        )
                    };
                    return self.push_call_frame(
                        ResolvedCallTarget {
                            code,
                            function,
                            environment,
                            kind,
                            layout,
                            strictness,
                        },
                        site,
                    );
                }
                FunctionExecutable::Native(NativeFunction::FunctionPrototype) => {
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(Immediate::Undefined),
                    );
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::NumberIsNaN
                    | NativeFunction::NumberIsFinite
                    | NativeFunction::NumberIsInteger
                    | NativeFunction::NumberIsSafeInteger),
                ) => {
                    let argument = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let result = numeric_value(argument).is_some_and(|number| match native {
                        NativeFunction::NumberIsNaN => number.is_nan(),
                        NativeFunction::NumberIsFinite => number.is_finite(),
                        NativeFunction::NumberIsInteger => {
                            number.is_finite() && number.fract() == 0.0
                        }
                        NativeFunction::NumberIsSafeInteger => {
                            number.is_finite()
                                && number.fract() == 0.0
                                && number.abs() <= crate::array::MAX_SAFE_INTEGER as f64
                        }
                        _ => unreachable!("numeric predicate dispatch is exhaustive"),
                    });
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if result {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::NumberValueOf) => {
                    let value = self.this_number_value(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::StringConstructor
                    | NativeFunction::NumberToExponential
                    | NativeFunction::NumberToFixed
                    | NativeFunction::NumberToPrecision
                    | NativeFunction::NumberToString
                    | NativeFunction::NumberConstructor),
                ) => return self.dispatch_conversion_native(native, &site, false),
                FunctionExecutable::Native(NativeFunction::ObjectConstructor) => {
                    let object = self.create_object_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, object);
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::SymbolConstructor
                    | NativeFunction::BooleanConstructor),
                ) => {
                    let value = self.primitive_constructor_value(native, &site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ObjectDefineProperty) => {
                    return self.object_define_property(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectGetOwnPropertyDescriptor) => {
                    return self.object_get_own_property_descriptor(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectGetOwnPropertyNames) => {
                    let result = self.object_get_own_property_names(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::ObjectHasOwnProperty) => {
                    return self.object_has_own_property(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectPropertyIsEnumerable) => {
                    return self.object_property_is_enumerable(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectHasOwn) => {
                    return self.object_has_own(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectIs) => {
                    let result = self.object_is(&site)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if result {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::ObjectGetPrototypeOf) => {
                    let prototype = self.object_get_prototype_of(&site)?;
                    return self.write(site.caller_base, site.destination, prototype);
                }
                FunctionExecutable::Native(NativeFunction::ObjectCreate) => {
                    let object = self.object_create(&site)?;
                    return self.write(site.caller_base, site.destination, object);
                }
                FunctionExecutable::Native(NativeFunction::ObjectIsPrototypeOf) => {
                    let result = self.object_is_prototype_of(&site)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if result {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::ObjectIsExtensible) => {
                    let result = self.object_is_extensible(&site)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if result {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::ObjectPreventExtensions) => {
                    let value = self.object_prevent_extensions(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ObjectToString) => {
                    let string = self.object_to_string(site.this_value)?;
                    return self.write(site.caller_base, site.destination, string);
                }
                FunctionExecutable::Native(NativeFunction::ObjectAssign) => {
                    let target = self.object_assign(&site)?;
                    return self.write(site.caller_base, site.destination, target);
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::ObjectKeys
                    | NativeFunction::ObjectValues
                    | NativeFunction::ObjectEntries),
                ) => {
                    let result = self.object_enumeration(&site, native)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::FunctionPrototypeCall) => {
                    let this_argument = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    site.callee = site.this_value;
                    site.this_value = this_argument;
                    if site.argument_count != 0 {
                        if site.argument_prefix_count != 0 {
                            site.argument_prefix_offset += 1;
                            site.argument_prefix_count -= 1;
                        } else {
                            site.argument_base = site.argument_base.checked_add(1).ok_or(
                                ExecutionError::RegisterWindowTooLarge(site.argument_count),
                            )?;
                        }
                        site.argument_count -= 1;
                    }
                }
                FunctionExecutable::Native(NativeFunction::FunctionPrototypeBind) => {
                    let bound = self.create_bound_function(&site)?;
                    return self.write(site.caller_base, site.destination, bound);
                }
                FunctionExecutable::Native(NativeFunction::FunctionConstructor) => {
                    return Err(ExecutionError::UnsupportedDynamicFunctionConstructor);
                }
                FunctionExecutable::Native(NativeFunction::ErrorConstructor(kind)) => {
                    let message = self.call_argument(&site, 0)?;
                    let error = self.create_native_error(kind, message)?;
                    return self.write(site.caller_base, site.destination, error);
                }
                FunctionExecutable::Native(NativeFunction::ArrayConstructor) => {
                    let array = self.create_array_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, array);
                }
                FunctionExecutable::Native(NativeFunction::ArrayIsArray) => {
                    let value = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let result = self.is_array_value(value)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if result {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::ArrayConcat) => {
                    let array = self.array_concat(&site)?;
                    return self.write(site.caller_base, site.destination, array);
                }
                FunctionExecutable::Native(NativeFunction::ArrayPush) => {
                    let length = self.array_push(&site)?;
                    return self.write(site.caller_base, site.destination, length);
                }
                FunctionExecutable::Native(NativeFunction::ArrayJoin) => {
                    let value = self.array_join(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayAt) => {
                    let value = self.array_at(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayIndexOf) => {
                    let value = self.array_search(&site, false)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayIncludes) => {
                    let value = self.array_search(&site, true)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayPop) => {
                    let value = self.array_pop(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArraySlice) => {
                    let value = self.array_slice(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayShift) => {
                    let value = self.array_shift(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayUnshift) => {
                    let value = self.array_unshift(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayReverse) => {
                    let value = self.array_reverse(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayFill) => {
                    let value = self.array_fill(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayLastIndexOf) => {
                    let value = self.array_last_index_of(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayCopyWithin) => {
                    let value = self.array_copy_within(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayFlat) => {
                    let value = self.array_flat(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArraySort) => {
                    let value = self.array_sort(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayToString) => {
                    let value = self.array_to_string(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayValues) => {
                    let iterator =
                        self.create_array_iterator(site.this_value, ArrayIterationKind::Value)?;
                    return self.write(site.caller_base, site.destination, iterator);
                }
                FunctionExecutable::Native(NativeFunction::ArrayIteratorNext) => {
                    match self.array_iterator_next_start(site.this_value)? {
                        ArrayIteratorNextAction::Done(result) => {
                            return self.write(site.caller_base, site.destination, result);
                        }
                        ArrayIteratorNextAction::Get {
                            iterator,
                            receiver,
                            callee,
                            mode,
                        } => {
                            return self
                                .dispatch_property_callback(
                                    NativeContinuation::array_iterator_property_get(
                                        NativeContinuationSite {
                                            caller_base: site.caller_base,
                                            destination: site.destination,
                                            call_site: site.call_site,
                                        },
                                        mode,
                                        iterator,
                                        receiver,
                                    ),
                                    callee,
                                )
                                .map(|_| ());
                        }
                    }
                }
                FunctionExecutable::Native(NativeFunction::IteratorIdentity) => {
                    return self.write(site.caller_base, site.destination, site.this_value);
                }
                FunctionExecutable::Native(native) if native.math_function().is_some() => {
                    let function = native
                        .math_function()
                        .expect("math guard establishes the native identity");
                    let value = self.math_value(function, &site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(native) if native.global_number_function().is_some() => {
                    let argument = self.call_argument(&site, 0)?;
                    if argument.is_some_and(|value| self.is_object_value(value)) {
                        return self.dispatch_conversion_native(native, &site, false);
                    }
                    let function = native
                        .global_number_function()
                        .expect("global-number guard establishes the native identity");
                    let value = self.global_number_value(function, &site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                _ => return Err(ExecutionError::UnsupportedDynamicFunctionConstructor),
            }
        }
    }

    #[inline(always)]
    pub(crate) fn resolve_function_object(
        &mut self,
        callee: Value,
    ) -> Result<FunctionObject, ExecutionError> {
        let raw = callee
            .as_heap_ref()
            .ok_or(ExecutionError::NonCallable(callee))?;
        self.heap.with_running_scope(|scope| {
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_raw_reference(raw, self.types.function)
                    .copied()
                    .map_err(|_| ExecutionError::NonCallable(callee))
            })
        })
    }

    /// Copies only callable dispatch metadata through a checked no-GC borrow on the hot path.
    #[inline(always)]
    fn resolve_function_executable(
        &mut self,
        callee: Value,
    ) -> Result<FunctionExecutable, ExecutionError> {
        let raw = callee
            .as_heap_ref()
            .ok_or(ExecutionError::NonCallable(callee))?;
        self.heap.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow_raw_reference(raw, self.types.function)
                .map(|function| function.executable)
                .map_err(|_| ExecutionError::NonCallable(callee))
        })
    }

    #[inline(always)]
    pub(crate) fn call_argument(
        &mut self,
        site: &CallSite,
        index: u32,
    ) -> Result<Option<Value>, ExecutionError> {
        if index >= site.argument_count {
            return Ok(None);
        }
        if index < site.argument_prefix_count {
            let data = site
                .argument_prefix
                .ok_or(ExecutionError::BoundArgumentCountOverflow)?;
            let index = site
                .argument_prefix_offset
                .checked_add(index)
                .ok_or(ExecutionError::BoundArgumentCountOverflow)?;
            return self.bound_function_argument(data, index).map(Some);
        }
        let suffix_index = index - site.argument_prefix_count;
        let absolute = site
            .argument_base
            .checked_add(suffix_index)
            .ok_or(ExecutionError::RegisterWindowTooLarge(site.argument_count))?;
        self.fiber
            .registers
            .get(absolute as usize)
            .copied()
            .map(Some)
            .ok_or(ExecutionError::InvalidRegister(RegisterId::new(
                suffix_index,
            )))
    }

    /// Reserves the callee state before mutation, then copies the supplied positional arguments.
    fn push_call_frame(
        &mut self,
        target: ResolvedCallTarget,
        site: CallSite,
    ) -> Result<(), ExecutionError> {
        if self.fiber.frames.len() >= self.stack_limits.max_frames as usize {
            return Err(ExecutionError::CallStackLimit {
                limit: self.stack_limits.max_frames,
            });
        }
        let register_count = target.layout.register_count;
        let callee_base = u32::try_from(self.fiber.registers.len())
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(register_count))?;
        let requested = callee_base
            .checked_add(register_count)
            .ok_or(ExecutionError::RegisterWindowTooLarge(register_count))?;
        if requested > self.stack_limits.max_registers {
            return Err(ExecutionError::RegisterStackLimit {
                limit: self.stack_limits.max_registers,
                requested,
            });
        }
        let additional = register_count as usize;
        if self.fiber.frames.len() == self.fiber.frames.capacity() {
            self.fiber
                .frames
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::FrameAllocationFailed)?;
        }
        if target.layout.max_handler_depth != 0 {
            let handler_depth = usize::try_from(target.layout.max_handler_depth).map_err(|_| {
                ExecutionError::HandlerStackTooLarge(target.layout.max_handler_depth)
            })?;
            if handler_depth > self.fiber.handlers.capacity() - self.fiber.handlers.len() {
                self.fiber
                    .handlers
                    .try_reserve_exact(handler_depth)
                    .map_err(|_| ExecutionError::HandlerAllocationFailed)?;
            }
        }
        if target.layout.max_completion_depth != 0 {
            let completion_depth =
                usize::try_from(target.layout.max_completion_depth).map_err(|_| {
                    ExecutionError::CompletionStackTooLarge(target.layout.max_completion_depth)
                })?;
            self.fiber
                .completions
                .reserve(completion_depth)
                .map_err(Self::completion_stack_error)?;
        }
        if additional > self.fiber.registers.capacity() - self.fiber.registers.len() {
            self.fiber
                .registers
                .try_reserve_exact(additional)
                .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        }
        self.fiber.registers.resize(
            requested as usize,
            Value::from_immediate(Immediate::Undefined),
        );
        let copied_arguments = site.argument_count.min(target.layout.argument_count);
        for index in 0..copied_arguments {
            let value = self
                .call_argument(&site, index)?
                .expect("copied argument index is within total count");
            self.write(callee_base, index, value)?;
        }
        let this_value = self.bind_ordinary_this(target.strictness, site.this_value);
        self.fiber.frames.push(Frame {
            code: target.code,
            function: target.function,
            pc: WordOffset::new(0),
            base: callee_base,
            environment: target.environment,
            return_register: Some(RegisterId::new(site.destination)),
            return_continuation: false,
            this_value,
            new_target: site.new_target,
            construct_receiver: site.construct_receiver,
            strictness: target.strictness,
            has_finally: target.layout.max_completion_depth != 0,
            argument_base: site.argument_base,
            argument_prefix: site.argument_prefix,
            argument_prefix_offset: site.argument_prefix_offset,
            argument_prefix_count: site.argument_prefix_count,
            argument_count: site.argument_count,
            handler_base: self.fiber.handlers.len() as u32,
            completion_base: self.fiber.completions.len() as u32,
            call_site: Some(site.call_site),
        });
        if let Some(slot_count) = NonZeroU32::new(target.layout.environment_slot_count)
            && let Err(error) = self.allocate_current_environment(target.kind, slot_count)
        {
            self.fiber.frames.pop();
            self.fiber.registers.truncate(callee_base as usize);
            return Err(error);
        }
        Ok(())
    }

    #[inline(always)]
    fn bind_ordinary_this(&self, strictness: FunctionStrictness, this_argument: Value) -> Value {
        if strictness == FunctionStrictness::Strict
            || !matches!(
                this_argument.as_immediate(),
                Some(Immediate::Undefined | Immediate::Null)
            )
        {
            return this_argument;
        }
        self.realm
            .global_object
            .expect("realm initialization publishes a global object")
    }

    /// Selects top-level completion or the hot ordinary-callee frame return path.
    #[inline(always)]
    fn finish_return(&mut self, value: Value) -> Result<Option<RunOutcome>, ExecutionError> {
        if self.fiber.frames.len() == 1 {
            return Ok(Some(RunOutcome::Completed(value)));
        }
        self.return_from_callee(value)
    }

    /// Pops a non-entry frame and restores caller checkpoints on the ordinary call hot path.
    #[inline(always)]
    fn return_from_callee(&mut self, value: Value) -> Result<Option<RunOutcome>, ExecutionError> {
        let frame = self
            .fiber
            .frames
            .pop()
            .expect("callee return always has an active frame");
        let value = match frame.construct_receiver {
            Some(receiver) if !self.is_object_value(value) => receiver,
            _ => value,
        };
        self.fiber.registers.truncate(frame.base as usize);
        self.fiber.handlers.truncate(frame.handler_base as usize);
        let continuation = if frame.return_continuation {
            Some(self.pop_native_continuation()?)
        } else {
            None
        };
        self.fiber
            .completions
            .truncate(frame.completion_base as usize);
        if let Some(continuation) = continuation {
            return self.resume_native_continuation(continuation, value);
        }
        let destination = frame
            .return_register
            .expect("non-entry frames always retain a caller destination");
        let caller_base = self
            .fiber
            .frames
            .last()
            .expect("a callee return with a destination retains its caller")
            .base;
        self.write(caller_base, destination.index(), value)?;
        Ok(None)
    }

    /// Resumes typed native work and drains synchronous parent continuations without Rust recursion.
    fn resume_native_continuation(
        &mut self,
        mut continuation: NativeContinuation,
        mut value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            if continuation.kind() == NativeContinuationKind::ConversionCallRoot {
                continuation = self.pop_native_continuation()?;
                if continuation.as_conversion().is_none() {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
                continue;
            }
            let site = continuation.site();
            let frame_depth = self.fiber.frames.len();
            let result = match continuation.kind() {
                NativeContinuationKind::Conversion { .. } => self.advance_native_conversion(
                    continuation
                        .as_conversion()
                        .expect("conversion kind reconstructs conversion continuation"),
                    Some(value),
                ),
                NativeContinuationKind::PropertyGet(mode) => {
                    if mode == PropertyCallbackMode::Descriptor {
                        let state =
                            self.pending_property_descriptor_reference(continuation.first())?;
                        self.write(
                            site.caller_base,
                            site.destination,
                            Value::from_heap_ref(state.raw()),
                        )?;
                        self.resume_property_descriptor(site, state, value)
                    } else if mode == PropertyCallbackMode::ArrayIteratorLength {
                        match self.array_iterator_resume_length(continuation.first(), value)? {
                            ArrayIteratorNextAction::Done(result) => {
                                self.write(site.caller_base, site.destination, result)?;
                                Ok(())
                            }
                            ArrayIteratorNextAction::Get {
                                callee,
                                mode,
                                receiver,
                                ..
                            } => self
                                .dispatch_property_callback(
                                    NativeContinuation::array_iterator_property_get(
                                        site,
                                        mode,
                                        continuation.first(),
                                        receiver,
                                    ),
                                    callee,
                                )
                                .map(|_| ()),
                        }
                    } else if mode == PropertyCallbackMode::ArrayIteratorElement {
                        let result =
                            self.array_iterator_resume_element(continuation.first(), value)?;
                        self.write(site.caller_base, site.destination, result)
                    } else {
                        self.write(site.caller_base, site.destination, value)
                    }
                }
                NativeContinuationKind::PropertySet => {
                    let receiver = continuation.first();
                    let assigned = continuation.second();
                    self.write(site.caller_base, site.destination, assigned)?;
                    self.finish_property_write(receiver, true)
                }
                NativeContinuationKind::ConversionCallRoot => {
                    unreachable!("conversion call roots resume before native dispatch")
                }
            };
            if let Err(error) = result {
                let Some(kind) = execution_error_kind(&error) else {
                    return Err(error);
                };
                return self.throw_native_error(kind, site.call_site);
            }
            if self.fiber.frames.len() != frame_depth {
                return Ok(None);
            }
            let Some(parent) = self.fiber.completions.pop_native() else {
                return Ok(None);
            };
            let parent_site = parent.site();
            value = self.read(parent_site.caller_base, parent_site.destination)?;
            continuation = parent;
        }
    }

    /// Propagates a thrown value through explicit frames until an immutable handler range matches.
    #[cold]
    #[inline(never)]
    fn throw_value(
        &mut self,
        value: Value,
        instruction_offset: WordOffset,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion = CompletionRecord::throw(value);
        debug_assert_eq!(completion.kind(), CompletionKind::Throw);
        self.dispatch_abrupt(completion, instruction_offset)
    }

    /// Iteratively routes one abrupt completion through handlers, finalizers, and explicit frames.
    #[cold]
    #[inline(never)]
    fn dispatch_abrupt(
        &mut self,
        mut completion: CompletionRecord,
        mut instruction_offset: WordOffset,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let frame = *self
                .fiber
                .frames
                .last()
                .expect("abrupt dispatch always has an active frame");
            self.fiber
                .completions
                .discard_native_suffix(frame.completion_base);
            if completion.kind() == CompletionKind::Throw
                && let Some(origin) =
                    self.suppress_iterator_close_error(frame, instruction_offset, &mut completion)?
            {
                instruction_offset = origin;
                continue;
            }
            let handler = self.find_abrupt_handler(frame, instruction_offset, completion)?;
            self.discard_exited_finalizers(frame, instruction_offset, completion, handler)?;
            if let Some((index, handler)) = handler {
                if handler.kind.is_finalizer() {
                    self.enter_finalizer(index, handler, completion)?;
                    return Ok(None);
                }
                debug_assert_eq!(completion.kind(), CompletionKind::Throw);
                let value = completion
                    .value()
                    .ok_or(ExecutionError::MissingCompletionRecord)?;
                let active = self
                    .fiber
                    .frames
                    .last_mut()
                    .expect("matched handler retains its frame");
                active.pc = handler.handler;
                self.fiber.pending_exception = Some(value);
                return Ok(None);
            }
            match completion.kind() {
                CompletionKind::Break | CompletionKind::Continue => {
                    let target = completion
                        .target()
                        .ok_or(ExecutionError::MissingCompletionRecord)?;
                    self.set_pc(target);
                    return Ok(None);
                }
                CompletionKind::Return => {
                    let value = completion
                        .value()
                        .ok_or(ExecutionError::MissingCompletionRecord)?;
                    return self.finish_return(value);
                }
                CompletionKind::Throw => {}
                CompletionKind::Normal => {
                    return Err(ExecutionError::MissingCompletionRecord);
                }
            }
            let value = completion
                .value()
                .ok_or(ExecutionError::MissingCompletionRecord)?;
            if self.fiber.frames.len() == 1 {
                return Ok(self.unhandled_throw(value));
            }
            let frame = self
                .fiber
                .frames
                .pop()
                .expect("non-entry abrupt completion retains a callee frame");
            self.fiber.registers.truncate(frame.base as usize);
            self.fiber.handlers.truncate(frame.handler_base as usize);
            self.fiber
                .completions
                .truncate(frame.completion_base as usize);
            instruction_offset = frame
                .call_site
                .expect("every non-entry frame records its caller call-site");
        }
    }

    /// Selects the innermost handler eligible for one completion kind and target.
    fn find_abrupt_handler(
        &self,
        frame: Frame,
        instruction_offset: WordOffset,
        completion: CompletionRecord,
    ) -> Result<Option<(u32, HandlerEntry)>, ExecutionError> {
        let function = self
            .loaded_code(frame.code)?
            .module
            .function(frame.function)
            .ok_or(ExecutionError::MissingEntryFunction(frame.function))?;
        for (index, handler) in function.handlers().iter().copied().enumerate().rev() {
            let covers_origin = handler.protected_start.index() <= instruction_offset.index()
                && instruction_offset.index() < handler.protected_end.index();
            if !covers_origin {
                continue;
            }
            let eligible = match completion.kind() {
                CompletionKind::Throw => true,
                CompletionKind::Return => handler.kind.is_finalizer(),
                CompletionKind::Break | CompletionKind::Continue => {
                    handler.kind.is_finalizer()
                        && completion.target().is_some_and(|target| {
                            target.index() < handler.protected_start.index()
                                || handler.protected_end.index() <= target.index()
                        })
                }
                CompletionKind::Normal => false,
            };
            if eligible {
                let index = u32::try_from(index)
                    .map_err(|_| ExecutionError::HandlerStackTooLarge(u32::MAX))?;
                return Ok(Some((index, handler)));
            }
        }
        Ok(None)
    }

    /// Selects the innermost finalizer covering a compiler-emitted normal exit.
    fn find_covering_finally(
        &self,
        instruction_offset: WordOffset,
    ) -> Result<Option<(u32, HandlerEntry)>, ExecutionError> {
        let frame = *self
            .fiber
            .frames
            .last()
            .expect("normal finalizer entry retains its frame");
        let function = self
            .loaded_code(frame.code)?
            .module
            .function(frame.function)
            .ok_or(ExecutionError::MissingEntryFunction(frame.function))?;
        for (index, handler) in function.handlers().iter().copied().enumerate().rev() {
            if handler.kind.is_finalizer()
                && handler.protected_start.index() <= instruction_offset.index()
                && instruction_offset.index() < handler.protected_end.index()
            {
                let index = u32::try_from(index)
                    .map_err(|_| ExecutionError::HandlerStackTooLarge(u32::MAX))?;
                return Ok(Some((index, handler)));
            }
        }
        Ok(None)
    }

    /// Publishes one saved completion and its active finalizer before redirecting the PC.
    fn enter_finalizer(
        &mut self,
        handler_index: u32,
        handler: HandlerEntry,
        completion: CompletionRecord,
    ) -> Result<(), ExecutionError> {
        if self.fiber.handlers.len() == self.fiber.handlers.capacity() {
            self.fiber
                .handlers
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::HandlerAllocationFailed)?;
        }
        self.fiber
            .completions
            .push_record(completion)
            .map_err(Self::completion_stack_error)?;
        let frame_depth = u32::try_from(self.fiber.frames.len())
            .map_err(|_| ExecutionError::HandlerStackTooLarge(u32::MAX))?;
        self.fiber.handlers.push(ActiveHandler {
            handler_index,
            frame_depth,
            environment_depth: handler.environment_depth,
        });
        self.set_pc(handler.handler);
        Ok(())
    }

    /// Restores an original throw when `IteratorClose` itself throws, per IteratorClose precedence.
    fn suppress_iterator_close_error(
        &mut self,
        frame: Frame,
        instruction_offset: WordOffset,
        completion: &mut CompletionRecord,
    ) -> Result<Option<WordOffset>, ExecutionError> {
        let Some(active) = self.fiber.handlers.last().copied() else {
            return Ok(None);
        };
        if active.frame_depth as usize != self.fiber.frames.len() {
            return Ok(None);
        }
        let handler = self.active_handler_entry(frame, active.handler_index)?;
        let inside_finalizer = handler.handler.index() <= instruction_offset.index()
            && instruction_offset.index() < handler.handler_end.index();
        if handler.kind != HandlerKind::IteratorClose || !inside_finalizer {
            return Ok(None);
        }
        let Some(saved) = self
            .fiber
            .completions
            .record(frame.completion_base as usize)
            .copied()
        else {
            return Err(ExecutionError::MissingCompletionRecord);
        };
        if saved.kind() != CompletionKind::Throw {
            return Ok(None);
        }
        self.fiber.handlers.pop();
        *completion = self
            .fiber
            .completions
            .restore_record(frame.completion_base)
            .ok_or(ExecutionError::MissingCompletionRecord)?;
        // Resume outside this handler's protected range so the original throw cannot re-enter the
        // same close finalizer; enclosing handlers still cover the finalizer's instruction range.
        Ok(Some(handler.handler))
    }

    /// Pops one verified active finalizer and replays its saved completion iteratively.
    fn resume_completion(
        &mut self,
        instruction_offset: WordOffset,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let frame = *self
            .fiber
            .frames
            .last()
            .expect("completion replay retains its frame");
        self.fiber
            .completions
            .discard_native_suffix(frame.completion_base);
        let active = self
            .fiber
            .handlers
            .pop()
            .ok_or(ExecutionError::MissingCompletionRecord)?;
        if active.frame_depth as usize != self.fiber.frames.len() {
            return Err(ExecutionError::MissingCompletionRecord);
        }
        let handler = self.active_handler_entry(frame, active.handler_index)?;
        if !(handler.handler.index() <= instruction_offset.index()
            && instruction_offset.index() < handler.handler_end.index())
        {
            return Err(ExecutionError::MissingCompletionRecord);
        }
        let completion = self
            .fiber
            .completions
            .restore_record(frame.completion_base)
            .ok_or(ExecutionError::MissingCompletionRecord)?;
        if completion.kind() == CompletionKind::Normal {
            return Ok(None);
        }
        self.dispatch_abrupt(completion, instruction_offset)
    }

    /// Removes saved completions only when the new control transfer exits their finalizer body.
    fn discard_exited_finalizers(
        &mut self,
        frame: Frame,
        instruction_offset: WordOffset,
        completion: CompletionRecord,
        candidate: Option<(u32, HandlerEntry)>,
    ) -> Result<(), ExecutionError> {
        loop {
            let Some(active) = self.fiber.handlers.last().copied() else {
                return Ok(());
            };
            if active.frame_depth as usize != self.fiber.frames.len() {
                return Ok(());
            }
            let handler = self.active_handler_entry(frame, active.handler_index)?;
            let origin_inside = handler.handler.index() <= instruction_offset.index()
                && instruction_offset.index() < handler.handler_end.index();
            let preserves = if !origin_inside {
                false
            } else {
                match completion.kind() {
                    CompletionKind::Break | CompletionKind::Continue => {
                        completion.target().is_some_and(|target| {
                            handler.handler.index() <= target.index()
                                && target.index() < handler.handler_end.index()
                        })
                    }
                    CompletionKind::Throw | CompletionKind::Return => {
                        candidate.is_some_and(|(_, nested)| {
                            handler.handler.index() <= nested.protected_start.index()
                                && nested.protected_end.index() <= handler.handler_end.index()
                        })
                    }
                    CompletionKind::Normal => true,
                }
            };
            if preserves {
                return Ok(());
            }
            self.fiber.handlers.pop();
            self.fiber
                .completions
                .restore_record(frame.completion_base)
                .ok_or(ExecutionError::MissingCompletionRecord)?;
        }
    }

    #[inline]
    fn active_handler_entry(
        &self,
        frame: Frame,
        handler_index: u32,
    ) -> Result<HandlerEntry, ExecutionError> {
        self.loaded_code(frame.code)?
            .module
            .function(frame.function)
            .and_then(|function| function.handlers().get(handler_index as usize))
            .copied()
            .ok_or(ExecutionError::MissingCompletionRecord)
    }

    #[inline]
    fn completion_stack_error(error: CompletionStackError) -> ExecutionError {
        match error {
            CompletionStackError::Limit { limit, requested } => {
                ExecutionError::CompletionStackLimit { limit, requested }
            }
            CompletionStackError::AllocationFailed => ExecutionError::CompletionAllocationFailed,
        }
    }

    /// Preserves the active fiber as the root owner until the host observes the unhandled value.
    #[cold]
    #[inline(never)]
    fn unhandled_throw(&mut self, value: Value) -> Option<RunOutcome> {
        Some(RunOutcome::Thrown(value))
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
            .reserve(completion_depth)
            .map_err(|error| match error {
                CompletionStackError::Limit { limit, requested } => {
                    ExecutionError::CompletionStackLimit { limit, requested }
                }
                CompletionStackError::AllocationFailed => {
                    ExecutionError::CompletionAllocationFailed
                }
            })?;
        Ok(())
    }

    #[inline(always)]
    pub(crate) fn read(&self, base: u32, register: u32) -> Result<Value, ExecutionError> {
        self.fiber
            .registers
            .get(base as usize + register as usize)
            .copied()
            .ok_or(ExecutionError::InvalidRegister(RegisterId::new(register)))
    }

    #[inline(always)]
    pub(crate) fn write(
        &mut self,
        base: u32,
        register: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
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

/// Executes allocation-free success branches while the verified cursor remains in local state.
///
/// # Safety
///
/// `instruction` must come from the verified function used to create `registers`; the register
/// backing must remain exclusively owned and must not change allocation or length until this
/// function returns. A `Slow` result ends that epoch before general isolate code runs.
#[inline(always)]
pub(crate) unsafe fn execute_verified_hot_instruction(
    registers: &mut RegisterWindow,
    instruction: DecodedInstruction,
    next_pc: &mut WordOffset,
) -> HotControl {
    let operands = instruction.operands;
    // SAFETY: The caller proves the verified operand and no-reallocation cursor invariants once for
    // this operation. Reads return Copy values and writes never expose references beyond the epoch.
    unsafe {
        match instruction.opcode {
            Opcode::Nop => HotControl::Continue,
            Opcode::LoadUndefined => {
                registers.write(operands[0], Value::from_immediate(Immediate::Undefined));
                HotControl::Continue
            }
            Opcode::LoadNull => {
                registers.write(operands[0], Value::from_immediate(Immediate::Null));
                HotControl::Continue
            }
            Opcode::LoadFalse => {
                registers.write(operands[0], Value::from_immediate(Immediate::False));
                HotControl::Continue
            }
            Opcode::LoadTrue => {
                registers.write(operands[0], Value::from_immediate(Immediate::True));
                HotControl::Continue
            }
            Opcode::LoadImmediate => {
                registers.write(operands[0], Value::from_i32(operands[1] as i32));
                HotControl::Continue
            }
            Opcode::Move => {
                let value = registers.read(operands[1]);
                registers.write(operands[0], value);
                HotControl::Continue
            }
            Opcode::Not => {
                let input = registers.read(operands[1]);
                if input.as_heap_ref().is_some() {
                    return HotControl::Slow;
                }
                let value = if is_non_string_truthy(input) {
                    Immediate::False
                } else {
                    Immediate::True
                };
                registers.write(operands[0], Value::from_immediate(value));
                HotControl::Continue
            }
            Opcode::Negate | Opcode::BitwiseNot | Opcode::ToNumber => {
                let input = registers.read(operands[1]);
                if numeric_value(input).is_none() {
                    return HotControl::Slow;
                }
                let value = match instruction.opcode {
                    Opcode::Negate => numeric_negate(input),
                    Opcode::BitwiseNot => numeric_bitwise_not(input),
                    Opcode::ToNumber => input,
                    _ => unreachable!("numeric unary hot dispatch is exhaustive"),
                };
                registers.write(operands[0], value);
                HotControl::Continue
            }
            Opcode::ToPropertyKey => {
                let key = registers.read(operands[1]);
                let guard = registers.read(operands[2]);
                if is_nullish(guard) || key.as_heap_ref().is_some() {
                    return HotControl::Slow;
                }
                registers.write(operands[0], key);
                HotControl::Continue
            }
            Opcode::Add => {
                let left = registers.read(operands[1]);
                let right = registers.read(operands[2]);
                let Some(value) = numeric_binary_hot(Opcode::Add, left, right) else {
                    return HotControl::Slow;
                };
                registers.write(operands[0], value);
                HotControl::Continue
            }
            Opcode::Sub | Opcode::Mul | Opcode::Div => {
                let left = registers.read(operands[1]);
                let right = registers.read(operands[2]);
                let Some(value) = numeric_binary_hot(instruction.opcode, left, right) else {
                    return HotControl::Slow;
                };
                registers.write(operands[0], value);
                HotControl::Continue
            }
            Opcode::BitwiseAnd
            | Opcode::BitwiseOr
            | Opcode::BitwiseXor
            | Opcode::ShiftLeft
            | Opcode::ShiftRight
            | Opcode::ShiftRightUnsigned
            | Opcode::Remainder
            | Opcode::Exponentiate => {
                let left = registers.read(operands[1]);
                let right = registers.read(operands[2]);
                if numeric_value(left).is_none() || numeric_value(right).is_none() {
                    return HotControl::Slow;
                }
                registers.write(
                    operands[0],
                    numeric_binary_operation(instruction.opcode, left, right),
                );
                HotControl::Continue
            }
            Opcode::LessThan | Opcode::GreaterThan | Opcode::LessEqual | Opcode::GreaterEqual => {
                let left = registers.read(operands[1]);
                let right = registers.read(operands[2]);
                let Some(value) = numeric_relational_hot(instruction.opcode, left, right) else {
                    return HotControl::Slow;
                };
                registers.write(operands[0], value);
                HotControl::Continue
            }
            Opcode::StrictEqual => {
                let left = registers.read(operands[1]);
                let right = registers.read(operands[2]);
                let Some(equal) = strict_equal_hot(left, right) else {
                    return HotControl::Slow;
                };
                registers.write(operands[0], boolean_value(equal));
                HotControl::Continue
            }
            Opcode::Jump => {
                *next_pc = WordOffset::new(operands[0]);
                HotControl::Continue
            }
            Opcode::JumpIfFalse | Opcode::JumpIfTrue => {
                let condition = registers.read(operands[0]);
                if condition.as_heap_ref().is_some() {
                    return HotControl::Slow;
                }
                let truthy = is_non_string_truthy(condition);
                if truthy == (instruction.opcode == Opcode::JumpIfTrue) {
                    *next_pc = WordOffset::new(operands[1]);
                }
                HotControl::Continue
            }
            Opcode::JumpIfNotNullish => {
                if !is_nullish(registers.read(operands[0])) {
                    *next_pc = WordOffset::new(operands[1]);
                }
                HotControl::Continue
            }
            _ => HotControl::Slow,
        }
    }
}
