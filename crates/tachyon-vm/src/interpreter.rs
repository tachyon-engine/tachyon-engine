//! Bytecode loading and explicit-fiber interpreter state machine.

use super::*;

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

    /// Converts the primitive values represented by the current numeric VM subset.
    #[inline(always)]
    pub(crate) fn convert_to_number(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if value.as_i32().is_some() || value.as_f64().is_some() {
            return Ok(value);
        }
        match value.as_immediate() {
            Some(Immediate::True) => Ok(Value::from_i32(1)),
            Some(Immediate::False | Immediate::Null) => Ok(Value::from_i32(0)),
            Some(Immediate::Undefined) => Ok(Value::from_f64(f64::NAN)),
            Some(Immediate::Hole | Immediate::Uninitialized) => {
                Err(ExecutionError::UnsupportedNumberConversion(value))
            }
            None => {
                let raw = value
                    .as_heap_ref()
                    .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
                if self.heap.checked_reference(raw, self.types.symbol).is_ok() {
                    return Err(ExecutionError::NotObject(value));
                }
                let Ok(reference) = self.heap.checked_reference(raw, self.types.string) else {
                    if self.is_object_value(value) {
                        let value_of = self.intern_intrinsic_name(b"valueOf")?;
                        let to_string = self.intern_intrinsic_name(b"toString")?;
                        let value_of = self.get_data_property(value, value_of)?;
                        let to_string = self.get_data_property(value, to_string)?;
                        let has_callable = [value_of, to_string]
                            .into_iter()
                            .flatten()
                            .any(|method| self.resolve_function_object(method).is_ok());
                        if !has_callable {
                            return Err(ExecutionError::NotObject(value));
                        }
                    }
                    return Err(ExecutionError::UnsupportedNumberConversion(value));
                };
                let units = self.heap.with_running_scope(|scope| {
                    let root = scope.root(reference).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        let string = no_gc
                            .borrow(root, self.types.string)
                            .map_err(ExecutionError::NoGcBorrow)?;
                        let units = match string.as_view() {
                            JsStringView::Latin1(bytes) => {
                                bytes.iter().map(|&byte| u16::from(byte)).collect()
                            }
                            JsStringView::Utf16(units) => units.to_vec(),
                        };
                        Ok::<_, ExecutionError>(units)
                    })
                })?;
                Ok(Value::from_f64(parse_number_code_units(&units)))
            }
        }
    }

    #[inline(always)]
    fn typeof_value(&self, value: Value) -> Result<Value, ExecutionError> {
        let strings = self.realm.typeof_strings;
        if value.as_i32().is_some() || value.as_f64().is_some() {
            return Ok(strings.number);
        }
        if let Some(immediate) = value.as_immediate() {
            return match immediate {
                Immediate::Undefined => Ok(strings.undefined),
                Immediate::Null => Ok(strings.object),
                Immediate::False | Immediate::True => Ok(strings.boolean),
                Immediate::Hole | Immediate::Uninitialized => {
                    Err(ExecutionError::UnsupportedTypeof(value))
                }
            };
        }
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedTypeof(value))?;
        if self.heap.checked_reference(raw, self.types.string).is_ok() {
            return Ok(strings.string);
        }
        if self.heap.checked_reference(raw, self.types.symbol).is_ok() {
            return Ok(strings.symbol);
        }
        if self
            .heap
            .checked_reference(raw, self.types.function)
            .is_ok()
        {
            return Ok(strings.function);
        }
        if self
            .heap
            .checked_reference(raw, self.types.ordinary_object)
            .is_ok()
        {
            return Ok(strings.object);
        }
        Err(ExecutionError::UnsupportedTypeof(value))
    }

    /// Implements SameValue, including NaN equality and signed-zero distinction.
    pub(crate) fn same_value(&mut self, left: Value, right: Value) -> Result<bool, ExecutionError> {
        if let (Some(left), Some(right)) = (numeric_value(left), numeric_value(right)) {
            if left.is_nan() && right.is_nan() {
                return Ok(true);
            }
            if left == 0.0 && right == 0.0 {
                return Ok(left.is_sign_negative() == right.is_sign_negative());
            }
            return Ok(left == right);
        }
        self.strict_equal_values(left, right)
    }

    /// Applies strict equality without allocating while preserving numeric and string semantics.
    pub(crate) fn strict_equal_values(
        &mut self,
        left: Value,
        right: Value,
    ) -> Result<bool, ExecutionError> {
        match (numeric_value(left), numeric_value(right)) {
            (Some(left), Some(right)) => return Ok(left == right),
            (Some(_), None) | (None, Some(_)) => return Ok(false),
            (None, None) => {}
        }
        if left == right {
            return Ok(true);
        }
        let (Some(left), Some(right)) = (left.as_heap_ref(), right.as_heap_ref()) else {
            return Ok(false);
        };
        let Ok(left) = self.heap.checked_reference(left, self.types.string) else {
            return Ok(false);
        };
        let Ok(right) = self.heap.checked_reference(right, self.types.string) else {
            return Ok(false);
        };
        self.heap.with_running_scope(|scope| {
            let left = scope.root(left).map_err(ExecutionError::Root)?;
            let right = scope.root(right).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let left = no_gc
                    .borrow(left, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let right = no_gc
                    .borrow(right, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(left == right)
            })
        })
    }

    /// Implements the supported primitive subset of Abstract Equality Comparison.
    fn loose_equal_values(&mut self, left: Value, right: Value) -> Result<bool, ExecutionError> {
        if self.strict_equal_values(left, right)? {
            return Ok(true);
        }
        let left_immediate = left.as_immediate();
        let right_immediate = right.as_immediate();
        let left_nullish = matches!(left_immediate, Some(Immediate::Undefined | Immediate::Null));
        let right_nullish = matches!(
            right_immediate,
            Some(Immediate::Undefined | Immediate::Null)
        );
        if left_nullish || right_nullish {
            return Ok(left_nullish && right_nullish);
        }
        let left_number = numeric_value(left);
        let right_number = numeric_value(right);
        if left_number.is_some() && right_number.is_some() {
            return Ok(left_number == right_number);
        }
        let left_boolean = matches!(left_immediate, Some(Immediate::True | Immediate::False));
        let right_boolean = matches!(right_immediate, Some(Immediate::True | Immediate::False));
        if left_boolean || right_boolean || left_number.is_some() || right_number.is_some() {
            let left = self.convert_to_number(left)?;
            let right = self.convert_to_number(right)?;
            return Ok(numeric_value(left) == numeric_value(right));
        }
        Ok(false)
    }

    #[inline(always)]
    pub(crate) fn is_truthy_value(&mut self, value: Value) -> Result<bool, ExecutionError> {
        if let Some(raw) = value.as_heap_ref()
            && let Ok(string) = self.heap.checked_reference(raw, self.types.string)
        {
            return self.heap.with_running_scope(|scope| {
                let string = scope.root(string).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(string, self.types.string)
                        .map(|string| !string.is_empty())
                        .map_err(ExecutionError::NoGcBorrow)
                })
            });
        }
        Ok(is_non_string_truthy(value))
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
                let key = self.property_key_atom(self.read(base, operands[1])?)?;
                let receiver = self.read(base, operands[2])?;
                let result = self.get_data_property(receiver, key)?.is_some();
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
                let key = self.property_key_atom(self.read(base, operands[2])?)?;
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
                let value = self
                    .get_data_property(receiver, key)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined));
                self.write(base, operands[0], value)?;
            }
            Opcode::SetById => {
                let receiver = self.read(base, operands[0])?;
                let value = self.read(base, operands[1])?;
                let key = self.scope_atom(code, operands[2])?;
                self.set_data_property_from_bytecode(receiver, key, value)?;
            }
            Opcode::GetByValue => {
                let receiver = self.read(base, operands[1])?;
                let key = self.property_key_atom(self.read(base, operands[2])?)?;
                let value = self
                    .get_data_property(receiver, key)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined));
                self.write(base, operands[0], value)?;
            }
            Opcode::SetByValue => {
                let receiver = self.read(base, operands[0])?;
                let value = self.read(base, operands[1])?;
                let key = self.property_key_atom(self.read(base, operands[2])?)?;
                self.set_data_property_from_bytecode(receiver, key, value)?;
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
                return self.finish_return(value);
            }
            Opcode::ReturnUndefined => {
                let value = Value::from_immediate(Immediate::Undefined);
                return self.finish_return(value);
            }
            Opcode::Throw => {
                let value = self.read(base, operands[0])?;
                return self.throw_value(value, instruction_offset);
            }
            _ => return Err(ExecutionError::UnsupportedOpcode(opcode)),
        }
        Ok(None)
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
                        .parent
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
                    .slots
                    .get(slot as usize)
                    .copied()
                    .ok_or(ExecutionError::InvalidEnvironmentSlot { depth, slot })
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
                let target = environment
                    .slots
                    .get_mut(slot as usize)
                    .ok_or(ExecutionError::InvalidEnvironmentSlot { depth, slot })?;
                *target = value;
                Ok::<(), ExecutionError>(())
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
        slot_count: NonZeroU32,
    ) -> Result<(), ExecutionError> {
        let slot_count = usize::try_from(slot_count.get())
            .map_err(|_| ExecutionError::EnvironmentStorageAllocationFailed)?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(slot_count)
            .map_err(|_| ExecutionError::EnvironmentStorageAllocationFailed)?;
        slots.resize(slot_count, Value::from_immediate(Immediate::Undefined));
        let parent = self.fiber.frames.last().and_then(|frame| frame.environment);
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
                Environment {
                    parent,
                    slots: slots.into_boxed_slice(),
                },
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
        self.allocate_current_environment(slot_count)
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
                    let (layout, strictness) = {
                        let function_template =
                            self.loaded_code(code)?
                                .module
                                .function(function)
                                .ok_or(ExecutionError::MissingEntryFunction(function))?;
                        (function_template.layout(), function_template.strictness())
                    };
                    return self.push_call_frame(
                        ResolvedCallTarget {
                            code,
                            function,
                            environment,
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
                    let object = self.object_define_property(&site)?;
                    return self.write(site.caller_base, site.destination, object);
                }
                FunctionExecutable::Native(NativeFunction::ObjectGetOwnPropertyDescriptor) => {
                    let result = self.object_get_own_property_descriptor(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::ObjectGetOwnPropertyNames) => {
                    let result = self.object_get_own_property_names(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::ObjectHasOwnProperty) => {
                    let result = self.object_has_own_property(&site)?;
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
                FunctionExecutable::Native(NativeFunction::ObjectPropertyIsEnumerable) => {
                    let enumerable = self.object_property_is_enumerable(&site)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if enumerable {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::ObjectHasOwn) => {
                    let result = self.object_has_own(&site)?;
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
                FunctionExecutable::Native(NativeFunction::MathPow) => {
                    let left = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let right = self
                        .call_argument(&site, 1)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let left = numeric_value(self.convert_to_number(left)?)
                        .ok_or(ExecutionError::UnsupportedNumberConversion(left))?;
                    let right = numeric_value(self.convert_to_number(right)?)
                        .ok_or(ExecutionError::UnsupportedNumberConversion(right))?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_f64(left.powf(right)),
                    );
                }
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
            && let Err(error) = self.allocate_current_environment(slot_count)
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

    /// Resumes native work after a callback return and maps language failures at the original call site.
    fn resume_native_continuation(
        &mut self,
        continuation: NativeContinuation,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match self.advance_native_conversion(continuation, Some(value)) {
            Ok(()) => Ok(None),
            Err(error) => {
                let Some(kind) = execution_error_kind(&error) else {
                    return Err(error);
                };
                self.throw_native_error(kind, continuation.site.call_site)
            }
        }
    }

    /// Propagates a thrown value through explicit frames until an immutable handler range matches.
    #[cold]
    #[inline(never)]
    fn throw_value(
        &mut self,
        value: Value,
        mut instruction_offset: WordOffset,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let frame = *self
                .fiber
                .frames
                .last()
                .expect("throw dispatch always has an active frame");
            if let Some(handler) = self.find_exception_handler(frame, instruction_offset)? {
                if handler.kind != HandlerKind::Catch {
                    return Err(ExecutionError::UnsupportedExceptionHandler(handler.kind));
                }
                let active = self
                    .fiber
                    .frames
                    .last_mut()
                    .expect("matched handler retains its frame");
                active.pc = handler.handler;
                self.fiber.handlers.truncate(frame.handler_base as usize);
                self.fiber
                    .completions
                    .truncate(frame.completion_base as usize);
                self.fiber.pending_exception = Some(value);
                return Ok(None);
            }
            if self.fiber.frames.len() == 1 {
                return Ok(self.unhandled_throw(value));
            }
            let frame = self
                .fiber
                .frames
                .pop()
                .expect("non-entry throw retains a callee frame");
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

    /// Selects the innermost half-open handler range for one verified function offset.
    #[inline]
    fn find_exception_handler(
        &self,
        frame: Frame,
        instruction_offset: WordOffset,
    ) -> Result<Option<HandlerEntry>, ExecutionError> {
        let function = self
            .loaded_code(frame.code)?
            .module
            .function(frame.function)
            .ok_or(ExecutionError::MissingEntryFunction(frame.function))?;
        Ok(function.handlers().iter().rev().copied().find(|handler| {
            handler.protected_start.index() <= instruction_offset.index()
                && instruction_offset.index() < handler.protected_end.index()
        }))
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
            .try_reserve_exact(completion_depth)
            .map_err(|_| ExecutionError::CompletionAllocationFailed)?;
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
