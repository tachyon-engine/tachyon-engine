//! Bytecode loading and explicit-fiber interpreter state machine.

use super::*;
use crate::property::TypedArrayIndexSetMode;

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

fn count_opcode(words: &[u32], target: Opcode) -> Result<usize, ExecutionError> {
    let mut count = 0_usize;
    let mut offset = WordOffset::new(0);
    while offset.index() < words.len() as u32 {
        let instruction = tachyon_bytecode::decode_instruction(words, offset)
            .map_err(|_| ExecutionError::DecodeInvariant(offset))?;
        count += usize::from(instruction.opcode == target);
        offset = WordOffset::new(offset.index() + u32::from(instruction.word_len));
    }
    Ok(count)
}

impl Isolate {
    /// Enumerates this isolate's fiber roots for a stop-the-world collection safepoint.
    ///
    /// The collector supplies a rewrite-capable tracer. This API does not resolve logical addresses
    /// or borrow heap objects, so it remains valid across non-moving collection phases.
    pub fn trace_roots(&mut self, tracer: &mut dyn Tracer) {
        self.fiber.trace_roots(tracer);
        self.finalization_jobs.trace(tracer);
        self.promise_jobs.trace(tracer);
        self.realm.trace(tracer);
        for (_, realm) in &mut self.inactive_realms {
            realm.trace(tracer);
        }
        for fiber in &mut self.suspended_fibers {
            fiber.trace_roots(tracer);
        }
        for code in &mut self.loaded_code {
            code.trace(tracer);
        }
        self.module_graph.trace(tracer);
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
            .position(|loaded| loaded.realm == self.active_realm && loaded.module.ptr_eq(module))
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
                            promise_jobs: &mut self.promise_jobs,
                            realm: &mut self.realm,
                            loaded_code: &mut self.loaded_code,
                            module_graph: &mut self.module_graph,
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
                BytecodeConstant::BigInt(decimal) => {
                    match self.allocate_bigint_code_constant(decimal, &mut constant_values) {
                        Ok(value) => Some(value),
                        Err(error) => {
                            self.atoms.rollback(checkpoint);
                            return Err(error);
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
            realm: self.active_realm,
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
    pub(crate) fn execute_loaded_with_batch<const N: usize>(
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
        budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        self.execute_loaded_loop_with_parent::<N, UNBOUNDED>(code, budget, None, None)
    }

    /// Runs eval code with an explicitly retained caller lexical environment.
    pub(crate) fn execute_loaded_with_parent(
        &mut self,
        code: CodeId,
        budget: ExecutionBudget,
        parent: Option<GcRef<Environment>>,
        eval_var_environment: Option<GcRef<Environment>>,
    ) -> Result<RunOutcome, ExecutionError> {
        self.execute_loaded_loop_with_parent::<{ tuning::dispatch::DEFAULT_DISPATCH_BATCH }, false>(
            code,
            budget,
            parent,
            eval_var_environment,
        )
    }

    /// Builds an exact direct-eval var overlay from verified declaration bytecode.
    pub(crate) fn prepare_direct_eval_var_environment(
        &mut self,
        code: CodeId,
        strict_eval: bool,
    ) -> Result<Option<GcRef<Environment>>, ExecutionError> {
        let caller = self
            .fiber
            .frames
            .last()
            .ok_or(ExecutionError::MissingEnvironment)?;
        let caller_kind = self
            .loaded_code(caller.code)?
            .module
            .function(caller.function)
            .ok_or(ExecutionError::MissingEntryFunction(caller.function))?
            .kind();
        if !strict_eval && matches!(caller_kind, FunctionKind::Script | FunctionKind::Module) {
            return Ok(None);
        }
        let declared = self.eval_declared_var_atoms(code)?;
        let frame_depth = self.fiber.frames.len() as u32;
        let previous = self
            .fiber
            .eval_var_environments
            .iter()
            .rev()
            .find(|environment| environment.frame_depth <= frame_depth)
            .map(|environment| environment.environment);
        let current = self
            .fiber
            .eval_var_environments
            .iter()
            .rev()
            .find(|environment| environment.frame_depth == frame_depth)
            .map(|environment| environment.environment);
        let ancestor = self
            .fiber
            .eval_var_environments
            .iter()
            .rev()
            .find(|environment| environment.frame_depth < frame_depth)
            .map(|environment| environment.environment);
        let lexical = self.fiber.frames.last().and_then(|frame| frame.environment);
        let mut names = Vec::new();
        names
            .try_reserve_exact(declared.len())
            .map_err(|_| ExecutionError::EnvironmentStorageAllocationFailed)?;
        for atom in declared {
            let lexical_binding = if !strict_eval {
                self.named_environment_binding(lexical, atom)?
            } else {
                None
            };
            if let Some((environment, slot)) = lexical_binding {
                if self.environment_slot_is_parameter(environment, slot)? {
                    return Err(ExecutionError::GlobalLexicalRedeclaration(atom));
                }
                let immutable = self.heap.with_running_scope(|scope| {
                    scope.with_no_gc_scope(|no_gc| {
                        no_gc
                            .borrow_reference(environment, self.types.environment)
                            .map_err(ExecutionError::NoGcBorrow)
                            .map(|record| record.slot_is_immutable(slot))
                    })
                })?;
                if immutable {
                    return Err(ExecutionError::GlobalLexicalRedeclaration(atom));
                }
            }
            let already_declared = !strict_eval
                && self
                    .eval_var_binding_until(current, atom, ancestor)?
                    .is_some()
                || lexical_binding.is_some();
            if !already_declared {
                names.push(atom);
            }
        }
        if names.is_empty() {
            return Ok(previous);
        }
        let environment = self.allocate_eval_var_environment(previous, names.into_boxed_slice())?;
        if !strict_eval {
            self.attach_eval_var_environment(frame_depth, environment)?;
        }
        Ok(Some(environment))
    }

    /// Scans verified entry bytecode twice so the declaration atom buffer has exact capacity.
    fn eval_declared_var_atoms(&self, code: CodeId) -> Result<Box<[AtomId]>, ExecutionError> {
        let loaded = self.loaded_code(code)?;
        let function_id = loaded.module.entry_function();
        let function = loaded
            .module
            .function(function_id)
            .ok_or(ExecutionError::MissingEntryFunction(function_id))?;
        let words = function.bytecode().bytecode().words();
        let count = count_opcode(words, Opcode::DeclareScope)?;
        let mut atoms = Vec::new();
        atoms
            .try_reserve_exact(count)
            .map_err(|_| ExecutionError::EnvironmentStorageAllocationFailed)?;
        let mut offset = WordOffset::new(0);
        while offset.index() < words.len() as u32 {
            let instruction = tachyon_bytecode::decode_instruction(words, offset)
                .map_err(|_| ExecutionError::DecodeInvariant(offset))?;
            if instruction.opcode == Opcode::DeclareScope {
                let scope_name = instruction.operands[0] as usize;
                let atom = loaded
                    .scope_resolutions
                    .get(scope_name)
                    .ok_or(ExecutionError::InvalidScopeName {
                        code,
                        scope_name: instruction.operands[0],
                    })?
                    .atom;
                atoms.push(atom);
            }
            offset = WordOffset::new(offset.index() + u32::from(instruction.word_len));
        }
        debug_assert_eq!(atoms.len(), count);
        Ok(atoms.into_boxed_slice())
    }

    /// Shares the dispatch loop while allowing only direct eval to seed an entry parent.
    fn execute_loaded_loop_with_parent<const N: usize, const UNBOUNDED: bool>(
        &mut self,
        code: CodeId,
        mut budget: ExecutionBudget,
        parent: Option<GcRef<Environment>>,
        eval_var_environment: Option<GcRef<Environment>>,
    ) -> Result<RunOutcome, ExecutionError> {
        let entry_function = self.loaded_code(code)?.module.entry_function();
        let dynamic_scope = parent.is_some() || eval_var_environment.is_some();
        self.enter_with_parent(code, entry_function, parent)?;
        self.fiber.dynamic_scope = dynamic_scope;
        self.fiber.direct_eval = dynamic_scope;
        if let Some(environment) = eval_var_environment {
            self.attach_eval_var_environment(1, environment)?;
        }
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
                    if let ExecutionError::HostThrown(value) = error {
                        match self.throw_value(value, instruction_offset) {
                            Ok(outcome) => outcome,
                            Err(error) => {
                                #[cfg(feature = "opcode-profile")]
                                self.execution_profile.record_fault_slow_exit();
                                return Err(error);
                            }
                        }
                    } else {
                        let Some(kind) = execution_error_kind(&error) else {
                            #[cfg(feature = "opcode-profile")]
                            self.execution_profile.record_fault_slow_exit();
                            self.cancel_signal_execution()?;
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
                let constant = self
                    .loaded_code(code)?
                    .module
                    .constants()
                    .get(constant_index)
                    .cloned()
                    .ok_or(ExecutionError::UnsupportedConstant(operands[1]))?;
                let value = match constant {
                    BytecodeConstant::NumberBits(bits) => Value::from_f64(f64::from_bits(bits)),
                    BytecodeConstant::String(_) | BytecodeConstant::BigInt(_) => self
                        .loaded_code(code)?
                        .constant_values
                        .get(constant_index)
                        .copied()
                        .flatten()
                        .ok_or(ExecutionError::UnsupportedConstant(operands[1]))?,
                    BytecodeConstant::RegExp { pattern, flags } => {
                        self.create_regexp_literal(&pattern, flags)?
                    }
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
                if self.is_bigint_value(input) {
                    let value = self.negate_bigint(input)?;
                    self.write(base, operands[0], value)?;
                } else if self.is_object_value(input) {
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
                    let value = self.numeric_primitive_bitwise_not(input)?;
                    self.write(base, operands[0], value)?;
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
                        let result =
                            self.numeric_primitive_binary_operation(opcode, left, right)?;
                        self.write(base, operands[0], result)?;
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
                let key_value = self.read(base, operands[1])?;
                let receiver = self.read(base, operands[2])?;
                return self.dispatch_has_property(
                    NativeContinuationSite {
                        caller_base: base,
                        destination: operands[0],
                        call_site: instruction_offset,
                    },
                    receiver,
                    key_value,
                );
            }
            Opcode::HasPrivate => {
                let name = self.read(base, operands[1])?;
                let receiver = self.read(base, operands[2])?;
                let present = self.has_private_element(receiver, name)?;
                self.write(
                    base,
                    operands[0],
                    Value::from_immediate(if present {
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
                let value = match self.dynamic_environment_value(resolution.atom)? {
                    Some(value) => value,
                    None => self
                        .scope_value(resolution)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined)),
                };
                let value = self.typeof_value(value)?;
                self.write(base, operands[0], value)?;
            }
            Opcode::DeleteById => {
                let key = self.scope_atom(code, operands[2])?;
                let receiver = self.read(base, operands[1])?;
                let mode = self.current_proxy_delete_mode();
                let site = NativeContinuationSite {
                    caller_base: base,
                    destination: operands[0],
                    call_site: instruction_offset,
                };
                if self.is_proxy_value(receiver) {
                    let key = self.atom_string_value(key)?;
                    let receiver = self.read(base, operands[1])?;
                    return self.dispatch_delete_property(site, receiver, key, mode);
                }
                return self.finish_ordinary_delete_property_key(site, receiver, key.into(), mode);
            }
            Opcode::DeleteByValue => {
                let key = self.property_key(self.read(base, operands[2])?)?;
                let receiver = self.read(base, operands[1])?;
                let mode = self.current_proxy_delete_mode();
                let site = NativeContinuationSite {
                    caller_base: base,
                    destination: operands[0],
                    call_site: instruction_offset,
                };
                if self.is_proxy_value(receiver) {
                    let key = match key {
                        PropertyKey::Atom(atom) => self.atom_string_value(atom)?,
                        PropertyKey::Symbol(symbol) => symbol.value(),
                        PropertyKey::Private(_) => {
                            return Err(ExecutionError::PrivatePropertyKeyEscaped);
                        }
                    };
                    let receiver = self.read(base, operands[1])?;
                    return self.dispatch_delete_property(site, receiver, key, mode);
                }
                return self.finish_ordinary_delete_property_key(site, receiver, key, mode);
            }
            Opcode::Typeof => {
                let value = self.typeof_value(self.read(base, operands[1])?)?;
                self.write(base, operands[0], value)?;
            }
            Opcode::InstanceOf => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                return self.begin_instance_of(
                    NativeContinuationSite {
                        caller_base: base,
                        destination: operands[0],
                        call_site: instruction_offset,
                    },
                    left,
                    right,
                );
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
                let value = match self.dynamic_environment_value(resolution.atom)? {
                    Some(value) => value,
                    None => self
                        .scope_value(resolution)?
                        .ok_or(ExecutionError::UnresolvedBinding(resolution.atom))?,
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::LoadIteratorSymbol => {
                let symbol = self
                    .realm
                    .well_known_symbols
                    .iterator
                    .expect("Symbol.iterator initializes before bytecode execution");
                self.write(base, operands[0], symbol)?;
            }
            Opcode::LoadAsyncIteratorSymbol => {
                let symbol = self
                    .realm
                    .well_known_symbols
                    .async_iterator
                    .expect("Symbol.asyncIterator initializes before bytecode execution");
                self.write(base, operands[0], symbol)?;
            }
            Opcode::CreateAsyncFromSyncIterator => {
                let iterator = self.read(base, operands[1])?;
                let next_method = self.read(base, operands[2])?;
                let wrapper = self.create_async_from_sync_iterator(iterator, next_method)?;
                self.write(base, operands[0], wrapper)?;
            }
            Opcode::CheckObject => {
                let value = self.read(base, operands[0])?;
                if !self.is_object_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
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
            Opcode::InitializeEnvironment => {
                let value = self.read(base, operands[0])?;
                self.initialize_environment(operands[1], operands[2], value)?;
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
            Opcode::CreateClass => self.create_derived_class(
                code,
                base,
                operands[0],
                FunctionId::new(operands[1]),
                operands[2],
            )?,
            Opcode::CreateBaseClass => {
                self.create_base_class(code, base, operands[0], FunctionId::new(operands[1]))?
            }
            Opcode::CheckConstructor => {
                let constructor = self.read(base, operands[0])?;
                if !self.is_constructor_value(constructor)? {
                    return Err(ExecutionError::NonConstructor(constructor));
                }
            }
            Opcode::SetFunctionName => {
                let function = self.read(base, operands[0])?;
                let name = self.scope_atom(code, operands[1])?;
                self.set_inferred_function_name(function, name)?;
            }
            Opcode::SetAccessorFunctionName => {
                let function = self.read(base, operands[0])?;
                let key = self.property_key(self.read(base, operands[1])?)?;
                self.set_accessor_function_name(function, key, operands[2] != 0)?;
            }
            Opcode::SetFunctionNameByValue => {
                let function = self.read(base, operands[0])?;
                let key = self.property_key(self.read(base, operands[1])?)?;
                self.set_method_function_name(function, key)?;
            }
            Opcode::SetFunctionHomeObject => {
                let function = self.read(base, operands[0])?;
                let home_object = self.read(base, operands[1])?;
                self.set_function_home_object(function, home_object)?;
            }
            Opcode::DefineFieldById
            | Opcode::DefineFieldByValue
            | Opcode::CreateDataPropertyById
            | Opcode::CreateDataPropertyByValue => {
                let target = self.read(base, operands[0])?;
                let value = self.read(base, operands[1])?;
                let key = if matches!(
                    opcode,
                    Opcode::DefineFieldById | Opcode::CreateDataPropertyById
                ) {
                    self.scope_atom(code, operands[2])?.into()
                } else {
                    self.property_key(self.read(base, operands[2])?)?
                };
                self.define_data_property(
                    target,
                    key,
                    DataPropertyDescriptor {
                        value: Some(value),
                        writable: Some(true),
                        enumerable: Some(true),
                        configurable: Some(true),
                    },
                )?;
            }
            Opcode::AttachInstanceFields => {
                self.attach_instance_fields(base, operands[0], operands[1], operands[2])?;
            }
            Opcode::CreatePrivateName => {
                let name = self.scope_atom(code, operands[1])?;
                let _ = name;
                let private_name = self.allocate_symbol(None)?;
                self.write(base, operands[0], private_name)?;
            }
            Opcode::GetPrivate => {
                let object = self.read(base, operands[1])?;
                let name = self.read(base, operands[2])?;
                match self.get_private_field(object, name)? {
                    PropertyRead::Data(value) => self.write(base, operands[0], value)?,
                    PropertyRead::Accessor(callee) => {
                        return self.dispatch_property_callback(
                            NativeContinuation::property_get(
                                NativeContinuationSite {
                                    caller_base: base,
                                    destination: operands[0],
                                    call_site: instruction_offset,
                                },
                                PropertyCallbackMode::Ordinary,
                                object,
                            ),
                            callee,
                        );
                    }
                    PropertyRead::Missing => {
                        return Err(ExecutionError::PrivateBrandCheckFailed(object));
                    }
                }
            }
            Opcode::SetPrivate => {
                let object = self.read(base, operands[0])?;
                let value = self.read(base, operands[1])?;
                let name = self.read(base, operands[2])?;
                match self.set_private_field(object, name, value)? {
                    PropertyWrite::Complete(true) => {}
                    PropertyWrite::Complete(false) => {
                        return Err(ExecutionError::ReadOnlyProperty(object));
                    }
                    PropertyWrite::Setter(callee) => {
                        return self.dispatch_property_callback(
                            NativeContinuation::property_set(
                                NativeContinuationSite {
                                    caller_base: base,
                                    destination: operands[1],
                                    call_site: instruction_offset,
                                },
                                object,
                                value,
                            ),
                            callee,
                        );
                    }
                }
            }
            Opcode::CreateAccessorPair => {
                let getter = self.read(base, operands[1])?;
                let setter = self.read(base, operands[2])?;
                let pair = self.allocate_private_accessor_pair(getter, setter)?;
                self.write(base, operands[0], pair)?;
            }
            Opcode::DefinePrivateField
            | Opcode::DefinePrivateMethod
            | Opcode::DefinePrivateAccessor => {
                let object = self.read(base, operands[0])?;
                let value = self.read(base, operands[1])?;
                let name = self.read(base, operands[2])?;
                match opcode {
                    Opcode::DefinePrivateField => {
                        self.define_private_field(object, name, value)?;
                    }
                    Opcode::DefinePrivateMethod => {
                        self.define_private_method(object, name, value)?;
                    }
                    Opcode::DefinePrivateAccessor => {
                        self.define_private_accessor(object, name, value)?;
                    }
                    _ => unreachable!(),
                }
            }
            Opcode::InitializeInstanceElements => {
                return self.initialize_instance_elements(NativeContinuationSite {
                    caller_base: base,
                    destination: operands[0],
                    call_site: instruction_offset,
                });
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
            Opcode::CreateExclusionList => {
                let list = self.create_exclusion_list(operands[1])?;
                self.write(base, operands[0], list)?;
            }
            Opcode::ExcludePropertyKey => {
                let list = self.read(base, operands[0])?;
                let key = self.read(base, operands[1])?;
                self.exclude_property_key(list, key)?;
            }
            Opcode::CopyDataProperties => {
                let target = self.read(base, operands[0])?;
                let source = self.read(base, operands[1])?;
                let exclusions = self.read(base, operands[2])?;
                return self.begin_copy_data_properties(
                    NativeContinuationSite {
                        caller_base: base,
                        destination: operands[0],
                        call_site: instruction_offset,
                    },
                    target,
                    source,
                    exclusions,
                );
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
                if value.as_immediate() == Some(Immediate::Uninitialized) {
                    return Err(ExecutionError::UninitializedThis);
                }
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
            Opcode::LoadArgumentsObject => {
                let arguments = self.materialize_arguments_object()?;
                self.write(base, operands[0], arguments)?;
            }
            Opcode::CollectRestArguments => {
                let rest = self.collect_rest_arguments(operands[1])?;
                self.write(base, operands[0], rest)?;
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
            Opcode::LoadSuperBase => {
                let (super_base, _) = self.current_super_reference()?;
                self.write(base, operands[0], super_base)?;
            }
            Opcode::GetSuperById => {
                let (super_base, receiver) = self.current_super_reference()?;
                let key = self.scope_atom(code, operands[1])?;
                return self.dispatch_reflect_property_read(
                    NativeContinuationSite {
                        caller_base: base,
                        destination: operands[0],
                        call_site: instruction_offset,
                    },
                    super_base,
                    receiver,
                    key.into(),
                );
            }
            Opcode::GetSuperByValue => {
                let super_base = self.read(base, operands[1])?;
                let receiver = self.current_this_receiver()?;
                let key = self.property_key(self.read(base, operands[2])?)?;
                return self.dispatch_reflect_property_read(
                    NativeContinuationSite {
                        caller_base: base,
                        destination: operands[0],
                        call_site: instruction_offset,
                    },
                    super_base,
                    receiver,
                    key,
                );
            }
            Opcode::EnterClassEnvironment => self.enter_class_environment(operands[0])?,
            Opcode::InitializeClassEnvironment => {
                let value = self.read(base, operands[0])?;
                self.initialize_class_environment(operands[1], value)?;
            }
            Opcode::LeaveClassEnvironment => self.leave_class_environment()?,
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
            Opcode::DefineClassMethodById | Opcode::DefineClassMethodByValue => {
                let target = self.read(base, operands[0])?;
                let method = self.read(base, operands[1])?;
                let key = if opcode == Opcode::DefineClassMethodById {
                    self.scope_atom(code, operands[2])?.into()
                } else {
                    self.property_key(self.read(base, operands[2])?)?
                };
                self.set_function_home_object(method, target)?;
                self.define_data_property(
                    target,
                    key,
                    DataPropertyDescriptor {
                        value: Some(method),
                        writable: Some(true),
                        enumerable: Some(false),
                        configurable: Some(true),
                    },
                )?;
            }
            Opcode::DefineClassGetterById
            | Opcode::DefineClassSetterById
            | Opcode::DefineClassGetterByValue
            | Opcode::DefineClassSetterByValue => {
                let receiver = self.read(base, operands[0])?;
                let function = self.read(base, operands[1])?;
                let key = if matches!(
                    opcode,
                    Opcode::DefineClassGetterById | Opcode::DefineClassSetterById
                ) {
                    self.scope_atom(code, operands[2])?.into()
                } else {
                    self.property_key(self.read(base, operands[2])?)?
                };
                self.set_function_home_object(function, receiver)?;
                let descriptor = if matches!(
                    opcode,
                    Opcode::DefineClassGetterById | Opcode::DefineClassGetterByValue
                ) {
                    AccessorPropertyDescriptor {
                        getter: Some(function),
                        setter: None,
                        enumerable: Some(false),
                        configurable: Some(true),
                    }
                } else {
                    AccessorPropertyDescriptor {
                        getter: None,
                        setter: Some(function),
                        enumerable: Some(false),
                        configurable: Some(true),
                    }
                };
                self.define_property(receiver, key, PropertyDescriptor::Accessor(descriptor))?;
            }
            Opcode::DefineGetterById | Opcode::DefineSetterById => {
                let receiver = self.read(base, operands[0])?;
                let function = self.read(base, operands[1])?;
                let key = self.scope_atom(code, operands[2])?;
                let descriptor = if opcode == Opcode::DefineGetterById {
                    AccessorPropertyDescriptor {
                        getter: Some(function),
                        setter: None,
                        enumerable: Some(true),
                        configurable: Some(true),
                    }
                } else {
                    AccessorPropertyDescriptor {
                        getter: None,
                        setter: Some(function),
                        enumerable: Some(true),
                        configurable: Some(true),
                    }
                };
                self.define_property(
                    receiver,
                    key.into(),
                    PropertyDescriptor::Accessor(descriptor),
                )?;
            }
            Opcode::DefineGetterByValue | Opcode::DefineSetterByValue => {
                let receiver = self.read(base, operands[0])?;
                let function = self.read(base, operands[1])?;
                let key = self.property_key(self.read(base, operands[2])?)?;
                let descriptor = if opcode == Opcode::DefineGetterByValue {
                    AccessorPropertyDescriptor {
                        getter: Some(function),
                        setter: None,
                        enumerable: Some(true),
                        configurable: Some(true),
                    }
                } else {
                    AccessorPropertyDescriptor {
                        getter: None,
                        setter: Some(function),
                        enumerable: Some(true),
                        configurable: Some(true),
                    }
                };
                self.define_property(receiver, key, PropertyDescriptor::Accessor(descriptor))?;
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
                    argument_source: None,
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
            Opcode::DirectEval => {
                let callee = self.read(base, operands[1])?;
                let is_current_eval = matches!(
                    self.resolve_function_executable(callee)?,
                    FunctionExecutable::Native(NativeFunction::HostEvalScript)
                ) && self.realm_for_callable(callee)? == self.active_realm;
                if is_current_eval {
                    let source = if operands[2] == 0 {
                        Value::from_immediate(Immediate::Undefined)
                    } else {
                        self.read(base, operands[1] + 1)?
                    };
                    if !self.is_string_value(source) {
                        self.write(base, operands[0], source)?;
                        return Ok(None);
                    }
                    let callback = self
                        .eval_script_callback
                        .ok_or(ExecutionError::UnsupportedDynamicFunctionConstructor)?;
                    let strict_caller = self
                        .fiber
                        .frames
                        .last()
                        .is_some_and(|frame| frame.strictness == FunctionStrictness::Strict);
                    let result = callback(
                        self,
                        self.active_realm,
                        EvalKind::Direct { strict_caller },
                        source,
                    )?;
                    self.write(base, operands[0], result)?;
                } else {
                    self.call(CallSite {
                        caller_base: base,
                        destination: operands[0],
                        callee,
                        argument_base: base
                            .checked_add(operands[1])
                            .and_then(|base| base.checked_add(1))
                            .ok_or(ExecutionError::RegisterWindowTooLarge(operands[2]))?,
                        argument_source: None,
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
            }
            Opcode::TailCall => {
                self.tail_call(CallSite {
                    caller_base: base,
                    destination: operands[0],
                    callee: self.read(base, operands[1])?,
                    argument_base: base
                        .checked_add(operands[1])
                        .and_then(|base| base.checked_add(1))
                        .ok_or(ExecutionError::RegisterWindowTooLarge(operands[2]))?,
                    argument_source: None,
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
                    argument_source: None,
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
            Opcode::TailCallWithReceiver => {
                let receiver = self.read(base, operands[1])?;
                let callee = self.read(base, operands[1] + 1)?;
                self.tail_call(CallSite {
                    caller_base: base,
                    destination: operands[0],
                    callee,
                    argument_base: base
                        .checked_add(operands[1])
                        .and_then(|base| base.checked_add(2))
                        .ok_or(ExecutionError::RegisterWindowTooLarge(operands[2]))?,
                    argument_source: None,
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
            Opcode::CallSpread
            | Opcode::TailCallSpread
            | Opcode::DirectEvalSpread
            | Opcode::CallSpreadWithReceiver
            | Opcode::TailCallSpreadWithReceiver => {
                let with_receiver = matches!(
                    opcode,
                    Opcode::CallSpreadWithReceiver | Opcode::TailCallSpreadWithReceiver
                );
                let receiver = if with_receiver {
                    self.read(base, operands[1])?
                } else {
                    Value::from_immediate(Immediate::Undefined)
                };
                let callee_register = operands[1] + u32::from(with_receiver);
                let callee = self.read(base, callee_register)?;
                let argument_list = self.read(base, operands[2])?;
                let operation = match opcode {
                    Opcode::TailCallSpread | Opcode::TailCallSpreadWithReceiver => {
                        ArgumentListOperation::TailCall
                    }
                    Opcode::DirectEvalSpread => ArgumentListOperation::DirectEval,
                    _ => ArgumentListOperation::Call,
                };
                return self
                    .begin_internal_spread_call(
                        &CallSite {
                            caller_base: base,
                            destination: operands[0],
                            callee,
                            argument_base: 0,
                            argument_source: None,
                            argument_prefix: None,
                            argument_prefix_offset: 0,
                            argument_prefix_count: 0,
                            argument_count: 0,
                            this_value: receiver,
                            new_target: Value::from_immediate(Immediate::Undefined),
                            construct_receiver: None,
                            call_site: instruction_offset,
                        },
                        argument_list,
                        callee,
                        receiver,
                        operation,
                    )
                    .map(|_| None);
            }
            Opcode::Construct => self.construct(
                base,
                operands[0],
                operands[1],
                operands[2],
                instruction_offset,
            )?,
            Opcode::SuperConstruct => self.super_construct(
                base,
                operands[0],
                operands[1],
                operands[2],
                instruction_offset,
            )?,
            Opcode::SuperConstructForwardAll => {
                self.super_construct_forward_all(base, operands[0], instruction_offset)?;
            }
            Opcode::InitializeThis => {
                let value = self.read(base, operands[0])?;
                self.initialize_derived_this(value)?;
            }
            Opcode::Return => {
                let value = self.read(base, operands[0])?;
                if !self
                    .fiber
                    .frames
                    .last()
                    .expect("return retains its frame")
                    .has_finally
                {
                    if self.fiber.frames.len() == 1
                        && !self
                            .fiber
                            .frames
                            .last()
                            .is_some_and(|frame| frame.return_continuation)
                    {
                        return self.promise_checkpoint(value, instruction_offset);
                    }
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
                    if self.fiber.frames.len() == 1
                        && !self
                            .fiber
                            .frames
                            .last()
                            .is_some_and(|frame| frame.return_continuation)
                    {
                        return self.promise_checkpoint(value, instruction_offset);
                    }
                    return self.finish_return(value);
                }
                return self
                    .dispatch_abrupt(CompletionRecord::return_value(value), instruction_offset);
            }
            Opcode::Throw => {
                let value = self.read(base, operands[0])?;
                return self.throw_value(value, instruction_offset);
            }
            Opcode::Await => {
                self.suspend_async_function_await(crate::async_function::AsyncAwaitSite {
                    code,
                    instruction: instruction_offset,
                    source: operands[0],
                    destination: operands[1],
                    suspend_id: operands[2],
                    base,
                })?;
                return Ok(None);
            }
            Opcode::Yield => {
                self.suspend_generator_yield(crate::generator::GeneratorSuspendSite {
                    code,
                    instruction: instruction_offset,
                    source: operands[0],
                    destination: operands[1],
                    kind_destination: None,
                    suspend_id: operands[2],
                    base,
                })?;
                return self.resume_restored_generator_native_caller();
            }
            Opcode::YieldDelegate => {
                self.suspend_generator_yield(crate::generator::GeneratorSuspendSite {
                    code,
                    instruction: instruction_offset,
                    source: operands[0],
                    destination: operands[1],
                    kind_destination: operands[1].checked_add(1),
                    suspend_id: operands[2],
                    base,
                })?;
                return self.resume_restored_generator_native_caller();
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
        self.dispatch_proxy_aware_property_read(
            NativeContinuationSite {
                caller_base,
                destination,
                call_site,
            },
            receiver,
            receiver,
            key,
        )
    }

    /// Executes Reflect.get with the target used for lookup and receiver used for an accessor call.
    pub(crate) fn dispatch_reflect_property_read(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.dispatch_proxy_aware_property_read(site, target, receiver, key)
    }

    #[inline(always)]
    fn current_proxy_delete_mode(&self) -> ProxyDeleteMode {
        match self
            .fiber
            .frames
            .last()
            .expect("property deletion always has an active frame")
            .strictness
        {
            FunctionStrictness::Sloppy => ProxyDeleteMode::Sloppy,
            FunctionStrictness::Strict => ProxyDeleteMode::Strict,
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
        if self.is_object_value(value) && self.is_typed_array_value(receiver) {
            match self.typed_array_index(key)? {
                crate::builtins::typed_array::TypedArrayIndex::NonNumeric => {}
                crate::builtins::typed_array::TypedArrayIndex::Invalid
                | crate::builtins::typed_array::TypedArrayIndex::Valid(_) => {
                    self.dispatch_typed_array_index_set_conversion(
                        NativeContinuationSite {
                            caller_base,
                            destination: value_register,
                            call_site,
                        },
                        receiver,
                        key,
                        value,
                        TypedArrayIndexSetMode::Assignment,
                    )?;
                    return Ok(None);
                }
            }
        }
        match self.resolve_property_write_until_proxy(receiver, key, value)? {
            PropertyWriteResolution::Proxy(proxy) => self.dispatch_proxy_aware_property_write(
                NativeContinuationSite {
                    caller_base,
                    destination: value_register,
                    call_site,
                },
                proxy,
                receiver,
                key,
                value,
                ProxySetMode::Assignment,
            ),
            PropertyWriteResolution::Write(write) => match write {
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
            },
        }
    }

    /// Executes Reflect.set without converting false ordinary-set results into strict errors.
    pub(crate) fn dispatch_reflect_property_write(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        if self.is_object_value(value) && self.is_typed_array_value(target) {
            let mode = match self.typed_array_index(key)? {
                crate::builtins::typed_array::TypedArrayIndex::NonNumeric => None,
                crate::builtins::typed_array::TypedArrayIndex::Invalid if target == receiver => {
                    Some(TypedArrayIndexSetMode::Reflect)
                }
                crate::builtins::typed_array::TypedArrayIndex::Invalid => None,
                crate::builtins::typed_array::TypedArrayIndex::Valid(_) if target == receiver => {
                    Some(TypedArrayIndexSetMode::Reflect)
                }
                crate::builtins::typed_array::TypedArrayIndex::Valid(_)
                    if self.is_typed_array_value(receiver)
                        && self.typed_array_index_get(target, key)?.flatten().is_some()
                        && self
                            .typed_array_index_get(receiver, key)?
                            .flatten()
                            .is_some() =>
                {
                    Some(TypedArrayIndexSetMode::ReflectReceiver)
                }
                crate::builtins::typed_array::TypedArrayIndex::Valid(_) => None,
            };
            if let Some(mode) = mode {
                self.dispatch_typed_array_index_set_conversion(site, receiver, key, value, mode)?;
                return Ok(None);
            }
        }
        match self.resolve_reflect_property_write_until_proxy(target, receiver, key, value)? {
            PropertyWriteResolution::Proxy(proxy) => self.dispatch_proxy_aware_property_write(
                site,
                proxy,
                receiver,
                key,
                value,
                ProxySetMode::Reflect,
            ),
            PropertyWriteResolution::Write(PropertyWrite::Complete(success)) => self
                .write(site.caller_base, site.destination, boolean_value(success))
                .map(|()| None),
            PropertyWriteResolution::Write(PropertyWrite::Setter(callee)) => self
                .dispatch_property_callback(
                    NativeContinuation::reflect_property_set(site, receiver, value),
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
        let (receiver, argument_base, argument_source, argument_count) = match continuation.kind() {
            NativeContinuationKind::PropertyGet(mode) => {
                let receiver = continuation.first();
                let receiver = if mode == PropertyCallbackMode::Descriptor {
                    let state = self.pending_property_descriptor_reference(receiver)?;
                    self.pending_property_descriptor_source(state)?
                } else if matches!(
                    mode,
                    PropertyCallbackMode::ArrayIteratorLength
                        | PropertyCallbackMode::ArrayIteratorElement
                        | PropertyCallbackMode::DefineProperties
                ) {
                    continuation.second()
                } else if mode == PropertyCallbackMode::CopyDataProperties {
                    let state =
                        self.pending_copy_data_properties_reference(continuation.first())?;
                    self.pending_copy_data_properties_source(state)?
                } else if mode == PropertyCallbackMode::ArgumentList {
                    let state = self.pending_argument_list_reference(continuation.first())?;
                    self.pending_argument_list_source(state)?
                } else {
                    receiver
                };
                (receiver, 0, None, 0)
            }
            NativeContinuationKind::PropertySet(mode) => {
                let receiver = if mode == PropertyWriteMode::ObjectAssign {
                    let state =
                        self.pending_copy_data_properties_reference(continuation.first())?;
                    self.pending_copy_data_properties_target(state)?
                } else {
                    continuation.first()
                };
                (
                    receiver,
                    site.caller_base
                        .checked_add(site.destination)
                        .ok_or(ExecutionError::RegisterWindowTooLarge(1))?,
                    None,
                    1,
                )
            }
            NativeContinuationKind::Proxy { stage, .. } => match stage {
                ProxyContinuationStage::TrapGetter => (continuation.second(), 0, None, 0),
                ProxyContinuationStage::TrapCall => {
                    let handler = self.proxy_snapshot(continuation.first())?.handler;
                    (
                        handler,
                        site.caller_base
                            .checked_add(site.destination)
                            .ok_or(ExecutionError::RegisterWindowTooLarge(1))?,
                        None,
                        1,
                    )
                }
                ProxyContinuationStage::ForwardResult => {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
            },
            NativeContinuationKind::ProxyCall { stage, .. } => match stage {
                ProxyCallStage::TrapGetter => (continuation.second(), 0, None, 0),
                ProxyCallStage::TrapCall => {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
            },
            NativeContinuationKind::ProxySetPrototype { stage, .. } => match stage {
                ProxySetPrototypeStage::TrapGetter | ProxySetPrototypeStage::TrapCall => {
                    let state = self.native_call_state_reference(continuation.first())?;
                    let pending = self.native_call_state_snapshot(state)?;
                    let proxy = pending.values[PROXY_ACTIVE_OBJECT];
                    let handler = self.proxy_snapshot(proxy)?.handler;
                    if stage == ProxySetPrototypeStage::TrapGetter {
                        (handler, 0, None, 0)
                    } else {
                        (handler, 0, Some(state), 2)
                    }
                }
                ProxySetPrototypeStage::TargetIsExtensible
                | ProxySetPrototypeStage::TargetGetPrototypeOf => {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
            },
            NativeContinuationKind::ProxySet { stage, .. } => {
                let state = self.native_call_state_reference(continuation.first())?;
                let pending = self.native_call_state_snapshot(state)?;
                let handler = self
                    .proxy_snapshot(pending.values[crate::proxy::SET_PROXY])?
                    .handler;
                match stage {
                    ProxySetStage::TrapGetter => (handler, 0, None, 0),
                    ProxySetStage::TrapCall => (handler, 0, Some(state), 4),
                    ProxySetStage::ReceiverGetOwn | ProxySetStage::ReceiverDefine => {
                        return Err(ExecutionError::MissingNativeContinuation);
                    }
                }
            }
            NativeContinuationKind::ProxyHas(stage) => {
                let state = self.native_call_state_reference(continuation.first())?;
                let pending = self.native_call_state_snapshot(state)?;
                let proxy = pending.values[PROXY_ACTIVE_OBJECT];
                let handler = self.proxy_snapshot(proxy)?.handler;
                match stage {
                    ProxyHasStage::TrapGetter => (handler, 0, None, 0),
                    ProxyHasStage::TrapCall => (handler, 0, Some(state), 2),
                    ProxyHasStage::TargetGetOwn | ProxyHasStage::TargetIsExtensible => {
                        return Err(ExecutionError::MissingNativeContinuation);
                    }
                }
            }
            NativeContinuationKind::ProxyGetOwn { stage, .. } => {
                let state = self.native_call_state_reference(continuation.first())?;
                let pending = self.native_call_state_snapshot(state)?;
                let proxy = pending.values[PROXY_ACTIVE_OBJECT];
                let handler = self.proxy_snapshot(proxy)?.handler;
                match stage {
                    ProxyGetOwnStage::TrapGetter => (handler, 0, None, 0),
                    ProxyGetOwnStage::TrapCall => (handler, 0, Some(state), 2),
                    ProxyGetOwnStage::TargetGetOwn | ProxyGetOwnStage::TargetIsExtensible => {
                        return Err(ExecutionError::MissingNativeContinuation);
                    }
                }
            }
            NativeContinuationKind::ProxyGet(stage) => {
                let state = self.native_call_state_reference(continuation.first())?;
                let pending = self.native_call_state_snapshot(state)?;
                match stage {
                    ProxyGetStage::TrapGetter => {
                        let handler = self
                            .proxy_snapshot(pending.values[PROXY_GET_ACTIVE])?
                            .handler;
                        (handler, 0, None, 0)
                    }
                    ProxyGetStage::TrapCall => {
                        let handler = self
                            .proxy_snapshot(pending.values[PROXY_GET_ACTIVE])?
                            .handler;
                        (handler, 0, Some(state), 3)
                    }
                    ProxyGetStage::TargetGetOwn => {
                        return Err(ExecutionError::MissingNativeContinuation);
                    }
                }
            }
            NativeContinuationKind::ProxyDelete { stage, .. } => {
                let state = self.native_call_state_reference(continuation.first())?;
                let pending = self.native_call_state_snapshot(state)?;
                let handler = self
                    .proxy_snapshot(pending.values[PROXY_DELETE_ACTIVE])?
                    .handler;
                match stage {
                    ProxyDeleteStage::TrapGetter => (handler, 0, None, 0),
                    ProxyDeleteStage::TrapCall => (handler, 0, Some(state), 2),
                    ProxyDeleteStage::TargetGetOwn | ProxyDeleteStage::TargetIsExtensible => {
                        return Err(ExecutionError::MissingNativeContinuation);
                    }
                }
            }
            NativeContinuationKind::ProxyDefine { stage, .. } => match stage {
                ProxyDefineStage::TrapGetter => {
                    let handler = self.pending_proxy_define_handler(continuation.first())?;
                    (handler, 0, None, 0)
                }
                ProxyDefineStage::TrapCall => {
                    let state = self.native_call_state_reference(continuation.first())?;
                    let handler =
                        self.native_call_state_snapshot(state)?.values[PROXY_DEFINE_HANDLER];
                    (handler, 0, Some(state), 3)
                }
                ProxyDefineStage::TargetGetOwn | ProxyDefineStage::TargetIsExtensible => {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
            },
            NativeContinuationKind::ProxyOwnKeys { stage, .. } => match stage {
                ProxyOwnKeysStage::TrapGetter => (continuation.second(), 0, None, 0),
                ProxyOwnKeysStage::TrapCall => {
                    let state = self.pending_proxy_own_keys_reference(continuation.first())?;
                    let pending = self.proxy_own_keys_snapshot_for_callback(state)?;
                    let handler = pending.handler;
                    let argument_base = site
                        .caller_base
                        .checked_add(site.destination)
                        .ok_or(ExecutionError::RegisterWindowTooLarge(1))?;
                    self.write(site.caller_base, site.destination, pending.target)?;
                    (handler, argument_base, None, 1)
                }
                ProxyOwnKeysStage::LengthGet | ProxyOwnKeysStage::ElementGet => {
                    let state = self.pending_proxy_own_keys_reference(continuation.first())?;
                    let pending = self.proxy_own_keys_snapshot_for_callback(state)?;
                    (pending.trap_result, 0, None, 0)
                }
                ProxyOwnKeysStage::TargetOwnKeys => {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
                ProxyOwnKeysStage::IntegrityExtensible | ProxyOwnKeysStage::IntegrityDescriptor => {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
            },
            NativeContinuationKind::CollectionInitializer(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::CollectionIteratorClose(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::CopyDataProperties(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::DefineProperties(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::GetOwnPropertyDescriptors(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::CollectionForEach => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ArrayForEach(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::TypedArrayCallback(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ArrayConcat(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ArrayFlat(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ArrayFlatMap(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ArrayCopy(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ArrayCopyWithin(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ArrayToSorted(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ArraySlice(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ArrayBufferSlice(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ArraySplice(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ArrayRemove(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ArrayInsert(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ArrayReverse(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ArrayFill(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ArrayJoin(_) => match continuation.kind() {
                NativeContinuationKind::ArrayJoin(ArrayJoinStage::ElementLocaleCall) => {
                    (continuation.second(), 0, None, 0)
                }
                _ => return Err(ExecutionError::MissingNativeContinuation),
            },
            NativeContinuationKind::ArrayStatic(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::MapGetOrInsertComputed => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::InstanceElements(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::InstanceOf => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ErrorConstructor(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ErrorToString(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ErrorStackSetter(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ObjectToString => (continuation.first(), 0, None, 0),
            NativeContinuationKind::ObjectIsPrototypeOf => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ObjectLookupAccessor { .. } => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ObjectToLocaleString(stage) => {
                if stage != ObjectToLocaleStringStage::Call {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
                (continuation.first(), 0, None, 0)
            }
            NativeContinuationKind::DateToJson(stage) => {
                if stage != DateToJsonStage::Call {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
                (continuation.first(), 0, None, 0)
            }
            NativeContinuationKind::RegExpTest(stage) => {
                if stage != RegExpTestStage::ExecCall {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
                let state = self.native_call_state_reference(continuation.first())?;
                (continuation.second(), 0, Some(state), 1)
            }
            NativeContinuationKind::RegExpSearch(stage) => {
                if !matches!(
                    stage,
                    RegExpSearchStage::StringMethodCall
                        | RegExpSearchStage::StringCreatedMethodCall
                        | RegExpSearchStage::ExecCall
                ) {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
                let state = self.native_call_state_reference(continuation.first())?;
                (continuation.second(), 0, Some(state), 1)
            }
            NativeContinuationKind::RegExpStringIterator(stage) => {
                if stage != RegExpStringIteratorStage::ExecCall {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
                let state = self.native_call_state_reference(continuation.first())?;
                (continuation.second(), 0, Some(state), 1)
            }
            NativeContinuationKind::RegExpFlags(_) => (continuation.second(), 0, None, 0),
            NativeContinuationKind::StringSplit(stage) => {
                if stage != StringSplitStage::SplitterCall {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
                let state = self.native_call_state_reference(continuation.first())?;
                (continuation.second(), 0, Some(state), 2)
            }
            NativeContinuationKind::StringReplaceAll(stage) => {
                if stage != StringReplaceAllStage::ReplaceCall {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
                let state = self.native_call_state_reference(continuation.first())?;
                (continuation.second(), 0, Some(state), 2)
            }
            NativeContinuationKind::TypedArrayConstruction(stage) => {
                let state =
                    self.pending_typed_array_construction_reference(continuation.first())?;
                let receiver = self.typed_array_construction_callback_receiver(state, stage)?;
                (receiver, 0, None, 0)
            }
            NativeContinuationKind::TypedArraySet(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::TypedArraySlice(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::TypedArraySubarray(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::JsonStringify(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::JsonParseReviver => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::SignalState(SignalStateStage::Equals) => {
                let state = self.native_call_state_reference(continuation.first())?;
                (continuation.second(), 0, Some(state), 2)
            }
            NativeContinuationKind::SignalWatcherHook => (continuation.second(), 0, None, 0),
            NativeContinuationKind::SignalState(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::SignalComputed => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::SignalUntrack => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::PromiseExecutor => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::PromiseReaction => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::PromiseCapabilityCall => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::PromiseThen(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::PromiseFinally => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::PromiseFinallyMethod(_) => (continuation.second(), 0, None, 0),
            NativeContinuationKind::PromiseCatch(_) => (continuation.second(), 0, None, 0),
            NativeContinuationKind::PromiseFinallyResolve => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::PromiseStaticResolve(
                PromiseStaticResolveStage::ConstructorPrototype,
            ) => (continuation.second(), 0, None, 0),
            NativeContinuationKind::PromiseStaticResolve(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::PromiseCombinator(_) => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::PromiseResolution(_) => (continuation.second(), 0, None, 0),
            NativeContinuationKind::PromiseThenable => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::FinalizationCleanup => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::Conversion { .. } => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::ConversionCallRoot => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::GeneratorResume => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::AsyncFunction => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            NativeContinuationKind::AsyncAwaitConstructor
            | NativeContinuationKind::AsyncFromSyncIterator(_) => {
                (continuation.second(), 0, None, 0)
            }
            NativeContinuationKind::AsyncFromSyncCloseOnReject(_) => {
                (continuation.first(), 0, None, 0)
            }
            NativeContinuationKind::RegExpReplace => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
        };
        // The continuation omits `callee` to stay 32 bytes: before frame publication it remains
        // reachable through the receiver's accessor pair (or descriptor state -> source chain).
        let completion_depth = self.fiber.completions.len();
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
            argument_source,
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
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
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
        if self.fiber.completions.len() <= completion_depth {
            return Ok(None);
        }
        let continuation = self.pop_native_continuation()?;
        let returned = self.read(site.caller_base, site.destination)?;
        self.resume_native_continuation(continuation, returned)
    }

    /// Dispatches one builtin object operation through Proxy exotic methods when applicable.
    #[cold]
    fn try_dispatch_proxy_builtin(
        &mut self,
        site: &CallSite,
        operation: ProxyInternalMethod,
    ) -> Result<bool, ExecutionError> {
        let Some(target) = self.call_argument(site, 0)? else {
            return Ok(false);
        };
        if !self.is_proxy_value(target) {
            return Ok(false);
        }
        self.dispatch_proxy_internal_method(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            target,
            operation,
        )?;
        Ok(true)
    }

    /// Dispatches Object/Reflect own-key consumers through the resumable Proxy list algorithm.
    #[cold]
    fn try_dispatch_proxy_own_keys(
        &mut self,
        site: &CallSite,
        mode: ProxyOwnKeysMode,
    ) -> Result<bool, ExecutionError> {
        let Some(target) = self.call_argument(site, 0)? else {
            return Ok(false);
        };
        if !self.is_proxy_value(target) {
            return Ok(false);
        }
        self.dispatch_proxy_own_keys(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            target,
            mode,
        )?;
        Ok(true)
    }

    /// Starts one callable Proxy trap after constructing its mandated argument array.
    fn dispatch_proxy_call_trap(
        &mut self,
        original: CallSite,
        proxy: Value,
        trap: Value,
        construct: bool,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.resolve_function_object(trap)?;
        let arguments = self.create_array_argument_list_from_site(&original)?;
        let state = self.allocate_proxy_call_state(
            proxy,
            arguments,
            original.this_value,
            original.new_target,
        )?;
        let site = NativeContinuationSite {
            caller_base: original.caller_base,
            destination: original.destination,
            call_site: original.call_site,
        };
        self.dispatch_proxy_call_trap_state(site, state, trap, construct)
    }

    /// Invokes an apply/construct trap using a previously materialized argument array.
    fn dispatch_proxy_call_trap_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        trap: Value,
        construct: bool,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.resolve_function_object(trap)?;
        let pending = self.native_call_state_snapshot(state)?;
        let proxy = pending.values[0];
        let snapshot = self.proxy_snapshot(proxy)?;
        let target = snapshot.target;
        let handler = snapshot.handler;
        let arguments = pending.values[1];
        let this_value = pending.values[2];
        let new_target = pending.values[3];
        let prefix_values = if construct {
            vec![target, arguments, new_target]
        } else {
            vec![target, this_value, arguments]
        };
        let undefined = Value::from_immediate(Immediate::Undefined);
        let prefix = self.create_apply_argument_prefix(target, handler, prefix_values)?;
        let continuation = NativeContinuation::proxy_call_trap(
            site,
            Value::from_heap_ref(state.raw()),
            trap,
            construct,
        );
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
        let trap_site = CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: trap,
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: 3,
            argument_count: 3,
            this_value: handler,
            new_target: undefined,
            construct_receiver: None,
            call_site: site.call_site,
        };
        if let Err(error) = self.call(trap_site) {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("a Proxy trap publishes its callee frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(None);
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        match self.resume_proxy_call(continuation, ProxyCallStage::TrapCall, value) {
            Ok(outcome) => Ok(outcome),
            Err(error) => match execution_error_kind(&error) {
                Some(kind) => self.throw_native_error(kind, site.call_site),
                None => Err(error),
            },
        }
    }

    /// Resumes a Proxy trap getter or validates the completed construct trap result.
    fn resume_proxy_call(
        &mut self,
        continuation: NativeContinuation,
        stage: ProxyCallStage,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let site = continuation.site();
        let NativeContinuationKind::ProxyCall { construct, .. } = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        match stage {
            ProxyCallStage::TrapGetter => {
                let state = self.native_call_state_reference(continuation.first())?;
                let pending = self.native_call_state_snapshot(state)?;
                if matches!(
                    value.as_immediate(),
                    Some(Immediate::Undefined | Immediate::Null)
                ) {
                    let target = self.proxy_snapshot(pending.values[0])?.target;
                    let call = CallSite {
                        caller_base: site.caller_base,
                        destination: site.destination,
                        callee: target,
                        argument_base: 0,
                        argument_source: None,
                        argument_prefix: None,
                        argument_prefix_offset: 0,
                        argument_prefix_count: 0,
                        argument_count: 0,
                        this_value: pending.values[2],
                        new_target: pending.values[3],
                        construct_receiver: None,
                        call_site: site.call_site,
                    };
                    let operation = if construct {
                        ArgumentListOperation::ReflectConstruct
                    } else {
                        ArgumentListOperation::ReflectApply
                    };
                    return self
                        .begin_argument_list(
                            &call,
                            pending.values[1],
                            target,
                            pending.values[2],
                            pending.values[3],
                            operation,
                        )
                        .map(|_| None);
                }
                self.dispatch_proxy_call_trap_state(site, state, value, construct)
            }
            ProxyCallStage::TrapCall => {
                if construct && !self.is_object_value(value) {
                    return self.throw_native_error(NativeErrorKind::Type, site.call_site);
                }
                self.write(site.caller_base, site.destination, value)?;
                Ok(None)
            }
        }
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
            argument_source: None,
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
    pub(crate) fn finish_property_write(
        &self,
        receiver: Value,
        success: bool,
    ) -> Result<(), ExecutionError> {
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
        let error_realm = self
            .fiber
            .frames
            .last()
            .and_then(|frame| self.loaded_code(frame.code).ok().map(|code| code.realm))
            .unwrap_or(self.active_realm);
        let error = self.create_native_error_in_realm(kind, None, error_realm)?;
        self.throw_value(error, instruction_offset)
    }

    #[inline(always)]
    pub(crate) fn scope_value(
        &mut self,
        resolution: ScopeResolution,
    ) -> Result<Option<Value>, ExecutionError> {
        if let Some(slot) = resolution.lexical_slot {
            return self.realm.lexical_value(slot).map(Some);
        }
        if resolution.intrinsic_slot.is_some() {
            let global = self
                .realm
                .global_object
                .expect("initialized realm publishes a global object");
            return self.get_data_property(global, resolution.atom);
        }
        Ok(resolution
            .global_slot
            .and_then(|slot| self.realm.get_slot(slot)))
    }

    /// Resolves a name through cold runtime environments used by direct eval and debugger code.
    fn dynamic_environment_binding(
        &mut self,
        atom: AtomId,
    ) -> Result<Option<(GcRef<Environment>, u32)>, ExecutionError> {
        if !self.fiber.dynamic_scope {
            return Ok(None);
        }
        if let Some(binding) = self.dynamic_eval_var_binding(atom)? {
            return Ok(Some(binding));
        }
        let cursor = self.fiber.frames.last().and_then(|frame| frame.environment);
        self.named_environment_binding(cursor, atom)
    }

    /// Resolves immutable owner metadata without consulting the Fiber dynamic-scope gate.
    fn named_environment_binding(
        &mut self,
        mut cursor: Option<GcRef<Environment>>,
        atom: AtomId,
    ) -> Result<Option<(GcRef<Environment>, u32)>, ExecutionError> {
        while let Some(environment) = cursor {
            let (owner, parent) = self.heap.with_running_scope(|scope| {
                scope.with_no_gc_scope(|no_gc| {
                    let environment = no_gc
                        .borrow_reference(environment, self.types.environment)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    Ok::<_, ExecutionError>((environment.owner(), environment.parent()))
                })
            })?;
            if let Some(owner) = owner {
                let function = self
                    .loaded_code(owner.code)?
                    .module
                    .function(owner.function)
                    .ok_or(ExecutionError::MissingEntryFunction(owner.function))?;
                let atom_string = self
                    .atoms
                    .get(atom)
                    .ok_or(ExecutionError::InvalidAtom(atom))?;
                if let Some(slot) = function
                    .environment_slots()
                    .iter()
                    .position(|metadata| atom_string.equals_str(&metadata.name))
                {
                    return Ok(Some((
                        environment,
                        u32::try_from(slot).map_err(|_| {
                            ExecutionError::InvalidEnvironmentSlot {
                                depth: 0,
                                slot: u32::MAX,
                            }
                        })?,
                    )));
                }
            }
            cursor = parent;
        }
        Ok(None)
    }

    /// Checks immutable owner metadata for a non-simple formal-parameter binding.
    fn environment_slot_is_parameter(
        &mut self,
        environment: GcRef<Environment>,
        slot: u32,
    ) -> Result<bool, ExecutionError> {
        let owner = self.heap.with_running_scope(|scope| {
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_reference(environment, self.types.environment)
                    .map_err(ExecutionError::NoGcBorrow)
                    .map(Environment::owner)
            })
        })?;
        let Some(owner) = owner else {
            return Ok(false);
        };
        let function = self
            .loaded_code(owner.code)?
            .module
            .function(owner.function)
            .ok_or(ExecutionError::MissingEntryFunction(owner.function))?;
        Ok(function
            .environment_slots()
            .get(slot as usize)
            .is_some_and(|metadata| metadata.parameter))
    }

    /// Walks only the sparse eval-var overlay chain owned by the active activation.
    fn dynamic_eval_var_binding(
        &mut self,
        atom: AtomId,
    ) -> Result<Option<(GcRef<Environment>, u32)>, ExecutionError> {
        let frame_depth = self.fiber.frames.len() as u32;
        let cursor = self
            .fiber
            .eval_var_environments
            .iter()
            .rev()
            .find(|environment| environment.frame_depth <= frame_depth)
            .map(|environment| environment.environment);
        self.eval_var_binding_from(cursor, atom)
    }

    fn eval_var_binding_from(
        &mut self,
        mut cursor: Option<GcRef<Environment>>,
        atom: AtomId,
    ) -> Result<Option<(GcRef<Environment>, u32)>, ExecutionError> {
        while let Some(environment) = cursor {
            let (slot, parent, kind) = self.heap.with_running_scope(|scope| {
                scope.with_no_gc_scope(|no_gc| {
                    let environment = no_gc
                        .borrow_reference(environment, self.types.environment)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    Ok::<_, ExecutionError>((
                        environment.dynamic_slot(atom),
                        environment.parent(),
                        environment.kind(),
                    ))
                })
            })?;
            if kind != EnvironmentKind::EvalVar {
                break;
            }
            if let Some(slot) = slot {
                return Ok(Some((environment, slot)));
            }
            cursor = parent;
        }
        Ok(None)
    }

    /// Checks declarations only within one variable environment, excluding ancestor overlays.
    fn eval_var_binding_until(
        &mut self,
        mut cursor: Option<GcRef<Environment>>,
        atom: AtomId,
        stop_before: Option<GcRef<Environment>>,
    ) -> Result<Option<(GcRef<Environment>, u32)>, ExecutionError> {
        while let Some(environment) = cursor {
            if Some(environment) == stop_before {
                break;
            }
            let (slot, parent) = self.heap.with_running_scope(|scope| {
                scope.with_no_gc_scope(|no_gc| {
                    let environment = no_gc
                        .borrow_reference(environment, self.types.environment)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    Ok::<_, ExecutionError>((environment.dynamic_slot(atom), environment.parent()))
                })
            })?;
            if let Some(slot) = slot {
                return Ok(Some((environment, slot)));
            }
            cursor = parent;
        }
        Ok(None)
    }

    fn dynamic_environment_value(&mut self, atom: AtomId) -> Result<Option<Value>, ExecutionError> {
        let Some((environment, slot)) = self.dynamic_environment_binding(atom)? else {
            return Ok(None);
        };
        self.heap.with_running_scope(|scope| {
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_reference(environment, self.types.environment)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .load(slot)
                    .map(Some)
                    .map_err(|error| environment_access_error(0, slot, error))
            })
        })
    }

    /// Writes one dynamically resolved slot and preserves the normal generational barrier.
    fn store_dynamic_environment(
        &mut self,
        atom: AtomId,
        value: Value,
    ) -> Result<bool, ExecutionError> {
        let Some((environment, slot)) = self.dynamic_environment_binding(atom)? else {
            return Ok(false);
        };
        self.heap.with_running_scope(|scope| {
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_reference_mut(environment, self.types.environment)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .store(slot, value)
                    .map_err(|error| environment_access_error(0, slot, error))
            })
        })?;
        if let Some(target) = value.as_heap_ref() {
            self.heap
                .write_barrier(environment.raw(), target)
                .map_err(ExecutionError::HeapReference)?;
        }
        Ok(true)
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
        if self.store_dynamic_environment(resolution.atom, value)? {
            return Ok(());
        }
        if resolution.intrinsic_slot.is_some() {
            let global = self
                .realm
                .global_object
                .expect("initialized realm publishes a global object");
            return self.set_own_data_property(global, resolution.atom, value);
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
        if self.dynamic_environment_binding(resolution.atom)?.is_some() {
            return Ok(());
        }
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
        if self.store_dynamic_environment(resolution.atom, value)? {
            return Ok(());
        }
        if let Some(slot) = resolution.lexical_slot {
            return self.realm.set_lexical(slot, value);
        }
        if resolution.intrinsic_slot.is_some() {
            let strict = self
                .fiber
                .frames
                .last()
                .is_some_and(|frame| frame.strictness == FunctionStrictness::Strict);
            let global = self
                .realm
                .global_object
                .expect("initialized realm publishes a global object");
            return match self.set_own_data_property(global, resolution.atom, value) {
                Err(ExecutionError::ReadOnlyProperty(_)) if !strict => Ok(()),
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

    /// Initializes one TDZ slot exactly once and applies the standard heap write barrier.
    fn initialize_environment(
        &mut self,
        depth: u32,
        slot: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let environment = self.environment_at_depth(depth)?;
        self.heap.with_running_scope(|scope| {
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_reference_mut(environment, self.types.environment)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .initialize(slot, value)
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

    /// Enters one exact immutable environment for class name and private-name identities.
    fn enter_class_environment(&mut self, slot_count: u32) -> Result<(), ExecutionError> {
        if self.fiber.class_environments.len() == self.fiber.class_environments.capacity() {
            self.fiber
                .class_environments
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::EnvironmentStorageAllocationFailed)?;
        }
        let parent = self.fiber.frames.last().and_then(|frame| frame.environment);
        let slot_count = NonZeroU32::new(slot_count)
            .ok_or(ExecutionError::EnvironmentStorageAllocationFailed)?;
        let environment = Environment::try_bindings(
            EnvironmentKind::Declarative,
            parent,
            None,
            slot_count,
            |_| BindingState::new(false, false),
        )
        .map_err(|_| ExecutionError::EnvironmentStorageAllocationFailed)?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
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
        let frame_depth = u32::try_from(self.fiber.frames.len())
            .map_err(|_| ExecutionError::EnvironmentStorageAllocationFailed)?;
        self.fiber
            .frames
            .last_mut()
            .ok_or(ExecutionError::MissingEnvironment)?
            .environment = Some(environment);
        self.fiber.class_environments.push(frame_depth);
        Ok(())
    }

    /// Initializes one active class-environment slot and publishes its managed edge.
    fn initialize_class_environment(
        &mut self,
        slot: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let frame_depth = self.fiber.frames.len();
        if !self
            .fiber
            .class_environments
            .last()
            .is_some_and(|depth| *depth as usize == frame_depth)
        {
            return Err(ExecutionError::MissingEnvironment);
        }
        let environment = self.environment_at_depth(0)?;
        self.heap.with_running_scope(|scope| {
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_reference_mut(environment, self.types.environment)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .initialize(slot, value)
                    .map_err(|error| environment_access_error(0, slot, error))
            })
        })?;
        if let Some(target) = value.as_heap_ref() {
            self.heap
                .write_barrier(environment.raw(), target)
                .map_err(ExecutionError::HeapReference)?;
        }
        Ok(())
    }

    /// Restores the parent environment after one class expression finishes or unwinds.
    fn leave_class_environment(&mut self) -> Result<(), ExecutionError> {
        let frame_depth = self.fiber.frames.len();
        if !self
            .fiber
            .class_environments
            .last()
            .is_some_and(|depth| *depth as usize == frame_depth)
        {
            return Err(ExecutionError::MissingEnvironment);
        }
        let environment = self
            .fiber
            .frames
            .last()
            .and_then(|frame| frame.environment)
            .ok_or(ExecutionError::MissingEnvironment)?;
        let parent = self.heap.with_running_scope(|scope| {
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_reference(environment, self.types.environment)
                    .map(Environment::parent)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        self.fiber
            .frames
            .last_mut()
            .expect("class environment retains its frame")
            .environment = parent;
        self.fiber.class_environments.pop();
        Ok(())
    }

    /// Restores the lexical depth frozen into the selected same-frame exception handler.
    fn restore_class_environment_depth(&mut self, target: u32) -> Result<(), ExecutionError> {
        let frame_depth = self.fiber.frames.len();
        let mut current = self
            .fiber
            .class_environments
            .iter()
            .rev()
            .take_while(|depth| **depth as usize == frame_depth)
            .count();
        let target = usize::try_from(target)
            .map_err(|_| ExecutionError::EnvironmentStorageAllocationFailed)?;
        if target > current {
            return Err(ExecutionError::MissingEnvironment);
        }
        while current > target {
            self.leave_class_environment()?;
            current -= 1;
        }
        Ok(())
    }

    /// Drops sparse environment roots after their owning JavaScript frame leaves the fiber.
    #[inline(always)]
    fn discard_exited_class_environments(&mut self) {
        let frame_depth = self.fiber.frames.len() as u32;
        while self
            .fiber
            .class_environments
            .last()
            .is_some_and(|depth| *depth > frame_depth)
        {
            self.fiber.class_environments.pop();
        }
        while self
            .fiber
            .eval_var_environments
            .last()
            .is_some_and(|environment| environment.frame_depth > frame_depth)
        {
            self.fiber.eval_var_environments.pop();
        }
        self.fiber.dynamic_scope =
            self.fiber.direct_eval || !self.fiber.eval_var_environments.is_empty();
    }

    /// Allocates non-empty captured-slot backing after the current activation frame is rooted.
    fn allocate_current_environment(
        &mut self,
        kind: FunctionKind,
        slot_count: NonZeroU32,
        self_binding: Option<(u32, Value)>,
    ) -> Result<(), ExecutionError> {
        let frame = self
            .fiber
            .frames
            .last()
            .expect("activation environment requires a rooted frame");
        let parent = frame.environment;
        let owner = EnvironmentOwner {
            code: frame.code,
            function: frame.function,
        };
        let environment =
            self.allocate_activation_environment(kind, parent, owner, slot_count, self_binding)?;
        self.fiber
            .frames
            .last_mut()
            .expect("environment allocation retains its frame")
            .environment = Some(environment);
        Ok(())
    }

    /// Builds and allocates one activation environment before its owning frame publishes it.
    fn allocate_activation_environment(
        &mut self,
        kind: FunctionKind,
        parent: Option<GcRef<Environment>>,
        owner: EnvironmentOwner,
        slot_count: NonZeroU32,
        self_binding: Option<(u32, Value)>,
    ) -> Result<GcRef<Environment>, ExecutionError> {
        let environment_kind = EnvironmentKind::for_activation(kind, parent.is_some());
        let metadata_states = if kind != FunctionKind::Module {
            let function = self
                .loaded_code(owner.code)?
                .module
                .function(owner.function)
                .ok_or(ExecutionError::MissingEntryFunction(owner.function))?;
            if function
                .environment_slots()
                .iter()
                .any(|slot| !slot.initialized || !slot.mutable)
            {
                let mut states = Vec::new();
                states
                    .try_reserve_exact(function.environment_slots().len())
                    .map_err(|_| ExecutionError::EnvironmentStorageAllocationFailed)?;
                for slot in function.environment_slots() {
                    states.push(BindingState::new(slot.mutable, slot.initialized));
                }
                Some(states.into_boxed_slice())
            } else {
                None
            }
        } else {
            None
        };
        let mut environment = if kind == FunctionKind::Module {
            Environment::try_bindings(environment_kind, parent, Some(owner), slot_count, |_| {
                BindingState::new(true, false)
            })
        } else if let Some(states) = metadata_states.as_ref() {
            Environment::try_bindings(environment_kind, parent, Some(owner), slot_count, |slot| {
                if self_binding.is_some_and(|(self_slot, _)| slot == self_slot) {
                    BindingState::new(false, false)
                } else {
                    states[slot as usize]
                }
            })
        } else {
            Environment::try_captured(environment_kind, parent, Some(owner), slot_count)
        }
        .map_err(|_| ExecutionError::EnvironmentStorageAllocationFailed)?;
        if kind == FunctionKind::Module {
            for slot in 0..slot_count.get() {
                environment
                    .initialize(slot, Value::from_immediate(Immediate::Undefined))
                    .expect("fresh module binding slots initialize exactly once");
            }
        } else if let Some((self_slot, value)) = self_binding {
            environment
                .initialize(self_slot, value)
                .expect("fresh named function binding initializes exactly once");
        }
        debug_assert_eq!(environment.kind(), environment_kind);
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.environment,
                0,
                environment,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates one exact eval-var record while every parent and atom remains isolate-rooted.
    fn allocate_eval_var_environment(
        &mut self,
        parent: Option<GcRef<Environment>>,
        names: Box<[AtomId]>,
    ) -> Result<GcRef<Environment>, ExecutionError> {
        let environment = Environment::try_dynamic(parent, names)
            .map_err(|_| ExecutionError::EnvironmentStorageAllocationFailed)?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.environment,
                0,
                environment,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Replaces one activation's overlay head or reserves exactly one sparse root entry.
    fn attach_eval_var_environment(
        &mut self,
        frame_depth: u32,
        environment: GcRef<Environment>,
    ) -> Result<(), ExecutionError> {
        if let Some(active) = self
            .fiber
            .eval_var_environments
            .iter_mut()
            .rev()
            .find(|active| active.frame_depth == frame_depth)
        {
            active.environment = environment;
        } else {
            self.fiber
                .eval_var_environments
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::EnvironmentStorageAllocationFailed)?;
            self.fiber.eval_var_environments.push(EvalVarEnvironment {
                frame_depth,
                environment,
            });
        }
        self.fiber.dynamic_scope = true;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn enter(
        &mut self,
        code: CodeId,
        function_id: FunctionId,
    ) -> Result<(), ExecutionError> {
        self.enter_with_parent(code, function_id, None)
    }

    /// Initializes one entry frame and optionally exposes a suspended direct-eval parent chain.
    fn enter_with_parent(
        &mut self,
        code: CodeId,
        function_id: FunctionId,
        parent: Option<GcRef<Environment>>,
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
        self.cancel_signal_execution()?;
        self.fiber.frames.clear();
        self.fiber.argument_objects.clear();
        self.fiber.argument_sources.clear();
        self.fiber.argument_callees.clear();
        self.fiber.derived_activations.clear();
        self.fiber.base_class_activations.clear();
        self.fiber.class_environments.clear();
        self.fiber.eval_var_environments.clear();
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
            environment: parent,
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
            receiver_or_home_object: None,
            call_site: None,
        });
        self.fiber.argument_objects.push(None);
        self.fiber.argument_sources.push(None);
        self.fiber.argument_callees.push(None);
        let Some(slot_count) = NonZeroU32::new(layout.environment_slot_count) else {
            return Ok(());
        };
        self.allocate_current_environment(kind, slot_count, None)
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
        let kind = self
            .loaded_code(code)?
            .module
            .function(function)
            .ok_or(ExecutionError::MissingEntryFunction(function))?
            .kind();
        let environment = self.fiber.frames.last().and_then(|frame| frame.environment);
        let internal_prototype = match kind {
            FunctionKind::Async => self
                .realm
                .async_function_prototype
                .expect("async function intrinsics initialize before bytecode execution"),
            FunctionKind::Generator => self
                .realm
                .generator_function_prototype
                .expect("generator intrinsics initialize before bytecode execution"),
            FunctionKind::AsyncGenerator => self
                .realm
                .async_generator_function_prototype
                .expect("async generator intrinsics initialize before bytecode execution"),
            _ => self
                .realm
                .function_prototype
                .expect("function intrinsics initialize before bytecode execution"),
        };
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
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
                    prototype_or_home_object: None,
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

    /// Creates a derived class constructor and its prototype pair from one evaluated heritage.
    fn create_derived_class(
        &mut self,
        code: CodeId,
        base: u32,
        destination: u32,
        function: FunctionId,
        superclass_register: u32,
    ) -> Result<(), ExecutionError> {
        let superclass = self.read(base, superclass_register)?;
        let superclass_prototype = self.read(
            base,
            superclass_register
                .checked_add(1)
                .ok_or(ExecutionError::InvalidRegister(RegisterId::new(u32::MAX)))?,
        )?;
        if superclass_prototype.as_immediate() != Some(Immediate::Null)
            && !self.is_object_value(superclass_prototype)
        {
            return Err(ExecutionError::NotObject(superclass_prototype));
        }
        self.create_closure(code, base, destination, function)?;
        let constructor = self.read(base, destination)?;
        self.set_function_internal_prototype(constructor, superclass)?;
        let prototype = self.create_ordinary_object_with_prototype(superclass_prototype)?;
        self.set_function_prototype(constructor, prototype)?;
        let constructor_atom = self.constructor_atom()?;
        self.define_data_property(
            prototype,
            constructor_atom,
            DataPropertyDescriptor {
                value: Some(constructor),
                writable: Some(true),
                enumerable: Some(false),
                configurable: Some(true),
            },
        )?;
        Ok(())
    }

    /// Creates a base class using the current realm's standard intrinsic prototype pair.
    fn create_base_class(
        &mut self,
        code: CodeId,
        base: u32,
        destination: u32,
        function: FunctionId,
    ) -> Result<(), ExecutionError> {
        let constructor_prototype = self
            .realm
            .function_prototype
            .expect("realm initialization publishes Function.prototype");
        let instance_prototype = self
            .realm
            .object_prototype
            .expect("realm initialization publishes Object.prototype");
        self.create_closure(code, base, destination, function)?;
        let constructor = self.read(base, destination)?;
        self.set_function_internal_prototype(constructor, constructor_prototype)?;
        let prototype = self.create_ordinary_object_with_prototype(instance_prototype)?;
        self.set_function_prototype(constructor, prototype)?;
        let constructor_atom = self.constructor_atom()?;
        self.define_data_property(
            prototype,
            constructor_atom,
            DataPropertyDescriptor {
                value: Some(constructor),
                writable: Some(true),
                enumerable: Some(false),
                configurable: Some(true),
            },
        )?;
        Ok(())
    }

    /// Freezes a verified register window into rare constructor metadata without growing hot objects.
    fn attach_instance_fields(
        &mut self,
        base: u32,
        constructor_register: u32,
        record_base: u32,
        count: u32,
    ) -> Result<(), ExecutionError> {
        let constructor = self.read(base, constructor_register)?;
        let function = self.resolve_function_object(constructor)?;
        let FunctionExecutable::Bytecode {
            code,
            function: function_id,
            environment,
        } = function.executable
        else {
            return Err(ExecutionError::InvalidClassFieldPlan);
        };
        let kind = self
            .loaded_code(code)?
            .module
            .function(function_id)
            .ok_or(ExecutionError::MissingEntryFunction(function_id))?
            .kind();
        if !matches!(
            kind,
            FunctionKind::DerivedClassConstructor | FunctionKind::BaseClassConstructor
        ) {
            return Err(ExecutionError::InvalidClassFieldPlan);
        }
        let mut records = Vec::new();
        records
            .try_reserve_exact(count as usize)
            .map_err(|_| ExecutionError::ClassFieldAllocationFailed)?;
        for index in 0..count {
            let offset = index
                .checked_mul(4)
                .ok_or(ExecutionError::InvalidClassFieldPlan)?;
            let key_value = self.read(
                base,
                record_base
                    .checked_add(offset)
                    .ok_or(ExecutionError::InvalidClassFieldPlan)?,
            )?;
            let payload = self.read(
                base,
                record_base
                    .checked_add(offset)
                    .and_then(|slot| slot.checked_add(1))
                    .ok_or(ExecutionError::InvalidClassFieldPlan)?,
            )?;
            let infer_name = self.read(
                base,
                record_base
                    .checked_add(offset)
                    .and_then(|slot| slot.checked_add(2))
                    .ok_or(ExecutionError::InvalidClassFieldPlan)?,
            )?;
            let kind = self.read(
                base,
                record_base
                    .checked_add(offset)
                    .and_then(|slot| slot.checked_add(3))
                    .ok_or(ExecutionError::InvalidClassFieldPlan)?,
            )?;
            let infer_name = match infer_name.as_immediate() {
                Some(Immediate::True) => true,
                Some(Immediate::False) => false,
                _ => return Err(ExecutionError::InvalidClassFieldPlan),
            };
            let kind = kind
                .as_i32()
                .and_then(|kind| u32::try_from(kind).ok())
                .and_then(ClassInstanceElementKind::from_operand)
                .ok_or(ExecutionError::InvalidClassFieldPlan)?;
            let payload = if payload.as_immediate() == Some(Immediate::Undefined) {
                None
            } else {
                let raw = payload
                    .as_heap_ref()
                    .ok_or(ExecutionError::InvalidClassFieldPlan)?;
                if kind == ClassInstanceElementKind::PrivateAccessor {
                    self.heap
                        .checked_reference(raw, self.types.accessor_pair)
                        .map_err(|_| ExecutionError::InvalidClassFieldPlan)?;
                } else {
                    self.resolve_function_object(payload)
                        .map_err(|_| ExecutionError::InvalidClassFieldPlan)?;
                }
                Some(payload)
            };
            if matches!(
                kind,
                ClassInstanceElementKind::PrivateMethod | ClassInstanceElementKind::PrivateAccessor
            ) && payload.is_none()
            {
                return Err(ExecutionError::InvalidClassFieldPlan);
            }
            records.push(ClassInstanceElementRecord {
                key: if kind.is_private() {
                    self.private_property_key(key_value)?
                } else {
                    self.property_key(key_value)?
                },
                payload,
                infer_name,
                kind,
            });
        }
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        let plan = self
            .heap
            .try_allocate_external_with_gc(
                self.types.class_instance_element_plan,
                0,
                ClassInstanceElementPlan {
                    records: records.into_boxed_slice(),
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        self.write(base, record_base, Value::from_heap_ref(plan.raw()))?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        let data = self
            .heap
            .try_allocate_with_gc(
                self.types.class_constructor_data,
                0,
                0,
                ClassConstructorData {
                    code,
                    function: function_id,
                    environment,
                    plan,
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let raw = constructor
            .as_heap_ref()
            .ok_or(ExecutionError::InvalidClassFieldPlan)?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.function)
            .map_err(|_| ExecutionError::InvalidClassFieldPlan)?;
        self.heap.with_running_scope(|scope| {
            let function = scope.root(reference).map_err(ExecutionError::Root)?;
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let function = no_gc
                    .borrow_mut(function, self.types.function)
                    .map_err(ExecutionError::NoGcBorrow)?;
                function.executable = FunctionExecutable::ClassBytecode(data.as_gc_ref());
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(function, Value::from_heap_ref(data.as_gc_ref().raw()))
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    /// Starts one resumable instance-element sequence after the constructor has bound `this`.
    fn initialize_instance_elements(
        &mut self,
        site: NativeContinuationSite,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let frame_depth = self.fiber.frames.len() as u32;
        let function = self
            .fiber
            .derived_activations
            .last()
            .filter(|activation| activation.frame_depth == frame_depth)
            .or_else(|| {
                self.fiber
                    .base_class_activations
                    .last()
                    .filter(|activation| activation.frame_depth == frame_depth)
            })
            .ok_or(ExecutionError::InvalidClassFieldPlan)?
            .function;
        let receiver = self.current_this_receiver()?;
        let executable = self.resolve_function_executable(function)?;
        let FunctionExecutable::ClassBytecode(data) = executable else {
            return Err(ExecutionError::InvalidClassFieldPlan);
        };
        let plan = self.class_constructor_snapshot(data)?.plan;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        let state = self
            .heap
            .try_allocate_with_gc(
                self.types.pending_instance_elements,
                0,
                0,
                PendingInstanceElements {
                    receiver,
                    plan,
                    index: 0,
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.advance_instance_elements(site, state)
    }

    /// Advances synchronously over empty fields and suspends only for initializer or Proxy calls.
    fn advance_instance_elements(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingInstanceElements>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let pending = self.pending_instance_elements_snapshot(state)?;
            let Some(record) = self.class_field_record(pending.plan, pending.index)? else {
                self.write(site.caller_base, site.destination, pending.receiver)?;
                return Ok(None);
            };
            if record.kind == ClassInstanceElementKind::PrivateMethod {
                self.define_private_method(
                    pending.receiver,
                    record
                        .key
                        .symbol_identity()
                        .map(SymbolId::value)
                        .ok_or(ExecutionError::InvalidClassFieldPlan)?,
                    record
                        .payload
                        .ok_or(ExecutionError::InvalidClassFieldPlan)?,
                )?;
                self.increment_instance_element_index(state)?;
                continue;
            }
            if record.kind == ClassInstanceElementKind::PrivateAccessor {
                self.define_private_accessor(
                    pending.receiver,
                    record
                        .key
                        .symbol_identity()
                        .map(SymbolId::value)
                        .ok_or(ExecutionError::InvalidClassFieldPlan)?,
                    record
                        .payload
                        .ok_or(ExecutionError::InvalidClassFieldPlan)?,
                )?;
                self.increment_instance_element_index(state)?;
                continue;
            }
            if let Some(initializer) = record.payload {
                self.push_instance_elements_continuation(
                    site,
                    state,
                    InstanceElementStage::Initializer,
                )?;
                let frame_depth = self.fiber.frames.len();
                if let Err(error) = self.call(CallSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    callee: initializer,
                    argument_base: 0,
                    argument_source: None,
                    argument_prefix: None,
                    argument_prefix_offset: 0,
                    argument_prefix_count: 0,
                    argument_count: 0,
                    this_value: pending.receiver,
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
                        .expect("field initializer publishes its bytecode frame");
                    frame.return_register = None;
                    frame.return_continuation = true;
                    return Ok(None);
                }
                let continuation = self.pop_native_continuation()?;
                let value = self.read(site.caller_base, site.destination)?;
                return self.resume_native_continuation(continuation, value);
            }
            if self.define_instance_field(
                site,
                state,
                pending.receiver,
                record,
                Value::from_immediate(Immediate::Undefined),
                false,
            )? {
                return Ok(None);
            }
        }
    }

    /// Continues after either a field initializer return or a completed Proxy define operation.
    fn resume_instance_elements(
        &mut self,
        continuation: NativeContinuation,
        stage: InstanceElementStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        let state = self.pending_instance_elements_reference(continuation.first())?;
        let pending = self.pending_instance_elements_snapshot(state)?;
        let record = self
            .class_field_record(pending.plan, pending.index)?
            .ok_or(ExecutionError::InvalidClassFieldPlan)?;
        match stage {
            InstanceElementStage::Initializer => {
                self.push_instance_elements_continuation(
                    site,
                    state,
                    InstanceElementStage::Define,
                )?;
                self.write(site.caller_base, site.destination, value)?;
                if record.infer_name {
                    self.set_method_function_name(value, record.key)?;
                }
                if self.define_instance_field(site, state, pending.receiver, record, value, true)? {
                    return Ok(());
                }
            }
            InstanceElementStage::Define => {
                self.increment_instance_element_index(state)?;
            }
        }
        self.advance_instance_elements(site, state).map(|_| ())
    }

    /// Defines one field directly or nests Proxy [[DefineOwnProperty]] under the field continuation.
    fn define_instance_field(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingInstanceElements>,
        receiver: Value,
        record: ClassInstanceElementRecord,
        value: Value,
        parent_pushed: bool,
    ) -> Result<bool, ExecutionError> {
        let descriptor = DataPropertyDescriptor {
            value: Some(value),
            writable: Some(true),
            enumerable: Some(true),
            configurable: Some(true),
        };
        if record.kind == ClassInstanceElementKind::PrivateField {
            self.define_private_field(
                receiver,
                record
                    .key
                    .symbol_identity()
                    .map(SymbolId::value)
                    .ok_or(ExecutionError::InvalidClassFieldPlan)?,
                value,
            )?;
            if parent_pushed {
                let continuation = self.pop_native_continuation()?;
                if continuation.kind()
                    != NativeContinuationKind::InstanceElements(InstanceElementStage::Define)
                {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
            }
            self.increment_instance_element_index(state)?;
            return Ok(false);
        }
        if !self.is_proxy_value(receiver) {
            self.define_data_property(receiver, record.key, descriptor)?;
            if parent_pushed {
                let continuation = self.pop_native_continuation()?;
                if continuation.kind()
                    != NativeContinuationKind::InstanceElements(InstanceElementStage::Define)
                {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
            }
            self.increment_instance_element_index(state)?;
            return Ok(false);
        }
        if !parent_pushed {
            self.push_instance_elements_continuation(site, state, InstanceElementStage::Define)?;
        }
        let frame_depth = self.fiber.frames.len();
        let result = self.dispatch_proxy_define(
            site,
            receiver,
            record.key,
            descriptor.into(),
            ProxyDefineMode::Object,
        );
        if let Err(error) = result {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            return Ok(true);
        }
        let continuation = self.pop_native_continuation()?;
        if continuation.kind()
            != NativeContinuationKind::InstanceElements(InstanceElementStage::Define)
        {
            return Err(ExecutionError::MissingNativeContinuation);
        }
        self.increment_instance_element_index(state)?;
        Ok(false)
    }

    fn push_instance_elements_continuation(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingInstanceElements>,
        stage: InstanceElementStage,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::instance_elements(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
            ))
            .map_err(|error| match error {
                CompletionStackError::Limit { limit, requested } => {
                    ExecutionError::CompletionStackLimit { limit, requested }
                }
                CompletionStackError::AllocationFailed => {
                    ExecutionError::CompletionAllocationFailed
                }
            })
    }

    fn pending_instance_elements_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingInstanceElements>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::InvalidClassFieldPlan)?;
        self.heap
            .checked_reference(raw, self.types.pending_instance_elements)
            .map_err(|_| ExecutionError::InvalidClassFieldPlan)
    }

    fn pending_instance_elements_snapshot(
        &mut self,
        state: GcRef<PendingInstanceElements>,
    ) -> Result<PendingInstanceElements, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_instance_elements)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn class_field_record(
        &mut self,
        plan: GcRef<ClassInstanceElementPlan>,
        index: u32,
    ) -> Result<Option<ClassInstanceElementRecord>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let plan = scope.root(plan).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(plan, self.types.class_instance_element_plan)
                    .map(|plan| plan.records.get(index as usize).copied())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn increment_instance_element_index(
        &mut self,
        state: GcRef<PendingInstanceElements>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let state = no_gc
                    .borrow_mut(state, self.types.pending_instance_elements)
                    .map_err(ExecutionError::NoGcBorrow)?;
                state.index = state
                    .index
                    .checked_add(1)
                    .ok_or(ExecutionError::InvalidClassFieldPlan)?;
                Ok(())
            })
        })
    }

    /// Resolves the active class function's dynamic super base and current `this` receiver.
    fn current_super_reference(&mut self) -> Result<(Value, Value), ExecutionError> {
        let receiver = self.current_this_receiver()?;
        let frame = self
            .fiber
            .frames
            .last()
            .copied()
            .ok_or(ExecutionError::UninitializedThis)?;
        let frame_depth = self.fiber.frames.len();
        let home_object = if frame.new_target.as_immediate() == Some(Immediate::Undefined) {
            frame
                .receiver_or_home_object
                .ok_or(ExecutionError::UninitializedThis)?
        } else {
            let function = self
                .fiber
                .derived_activations
                .last()
                .filter(|activation| activation.frame_depth as usize == frame_depth)
                .or_else(|| {
                    self.fiber
                        .base_class_activations
                        .last()
                        .filter(|activation| activation.frame_depth as usize == frame_depth)
                })
                .map(|activation| activation.function)
                .ok_or(ExecutionError::UninitializedThis)?;
            self.function_home_object(function)?
        };
        let super_base = self.object_snapshot(home_object)?.1.prototype;
        if super_base.as_immediate() == Some(Immediate::Null) {
            return Err(ExecutionError::NotObject(super_base));
        }
        Ok((super_base, receiver))
    }

    /// Reads the active frame's `this` without re-observing a computed super base.
    #[inline(always)]
    fn current_this_receiver(&self) -> Result<Value, ExecutionError> {
        let receiver = self
            .fiber
            .frames
            .last()
            .ok_or(ExecutionError::UninitializedThis)?
            .this_value;
        if receiver.as_immediate() == Some(Immediate::Uninitialized) {
            return Err(ExecutionError::UninitializedThis);
        }
        Ok(receiver)
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
        self.construct_site(CallSite {
            caller_base,
            destination,
            callee: constructor,
            argument_base: caller_base
                .checked_add(callee_register)
                .and_then(|base| base.checked_add(1))
                .ok_or(ExecutionError::RegisterWindowTooLarge(argument_count))?,
            argument_source: None,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: constructor,
            construct_receiver: None,
            call_site,
        })
    }

    /// Constructs the dynamically current superclass while forwarding the active `new.target`.
    fn super_construct(
        &mut self,
        caller_base: u32,
        destination: u32,
        argument_base_register: u32,
        argument_count: u32,
        call_site: WordOffset,
    ) -> Result<(), ExecutionError> {
        let activation = self
            .fiber
            .derived_activations
            .last()
            .copied()
            .filter(|activation| activation.frame_depth as usize == self.fiber.frames.len())
            .ok_or(ExecutionError::UninitializedThis)?;
        let superclass = self.object_snapshot(activation.function)?.1.prototype;
        if !self.is_constructor_value(superclass)? {
            return Err(ExecutionError::NonConstructor(superclass));
        }
        let new_target = self
            .fiber
            .frames
            .last()
            .expect("super construction retains its derived frame")
            .new_target;
        self.construct_site(CallSite {
            caller_base,
            destination,
            callee: superclass,
            argument_base: caller_base
                .checked_add(argument_base_register)
                .and_then(|base| base.checked_add(1))
                .ok_or(ExecutionError::RegisterWindowTooLarge(argument_count))?,
            argument_source: None,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target,
            construct_receiver: None,
            call_site,
        })
    }

    /// Constructs the current superclass while forwarding the active frame's complete argument view.
    fn super_construct_forward_all(
        &mut self,
        caller_base: u32,
        destination: u32,
        call_site: WordOffset,
    ) -> Result<(), ExecutionError> {
        let activation = self
            .fiber
            .derived_activations
            .last()
            .copied()
            .filter(|activation| activation.frame_depth as usize == self.fiber.frames.len())
            .ok_or(ExecutionError::UninitializedThis)?;
        let superclass = self.object_snapshot(activation.function)?.1.prototype;
        if !self.is_constructor_value(superclass)? {
            return Err(ExecutionError::NonConstructor(superclass));
        }
        let frame = *self
            .fiber
            .frames
            .last()
            .expect("default derived constructor retains its active frame");
        let argument_source = self.fiber.argument_sources.last().copied().flatten();
        self.construct_site(CallSite {
            caller_base,
            destination,
            callee: superclass,
            argument_base: frame.argument_base,
            argument_source,
            argument_prefix: frame.argument_prefix,
            argument_prefix_offset: frame.argument_prefix_offset,
            argument_prefix_count: frame.argument_prefix_count,
            argument_count: frame.argument_count,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: frame.new_target,
            construct_receiver: None,
            call_site,
        })
    }

    /// Binds the object returned by `super()` exactly once to the active derived constructor.
    fn initialize_derived_this(&mut self, value: Value) -> Result<(), ExecutionError> {
        if !self.is_object_value(value) {
            return Err(ExecutionError::NotObject(value));
        }
        let is_derived = self
            .fiber
            .derived_activations
            .last()
            .is_some_and(|activation| activation.frame_depth as usize == self.fiber.frames.len());
        if !is_derived {
            return Err(ExecutionError::UninitializedThis);
        }
        let frame = self
            .fiber
            .frames
            .last_mut()
            .expect("derived this initialization retains its frame");
        if frame.this_value.as_immediate() != Some(Immediate::Uninitialized) {
            return Err(ExecutionError::SuperAlreadyCalled);
        }
        frame.this_value = value;
        frame.receiver_or_home_object = Some(value);
        Ok(())
    }

    /// Drives a pre-built construct call site through bound forwarding and receiver allocation.
    pub(crate) fn construct_site(&mut self, mut site: CallSite) -> Result<(), ExecutionError> {
        let derived = loop {
            if self.is_proxy_value(site.callee) {
                let proxy = site.callee;
                let snapshot = self.proxy_snapshot(proxy)?;
                if snapshot.handler.as_immediate() == Some(Immediate::Null) {
                    return Err(ExecutionError::ProxyRevoked);
                }
                if !self.is_constructor_value(snapshot.target)? {
                    return Err(ExecutionError::NonConstructor(snapshot.target));
                }
                let construct_atom = self.intern_intrinsic_name(b"construct")?;
                match self.resolve_property_read(snapshot.handler, construct_atom.into())? {
                    PropertyRead::Missing => {
                        site.callee = snapshot.target;
                        continue;
                    }
                    PropertyRead::Data(value)
                        if value.as_immediate() == Some(Immediate::Undefined)
                            || value.as_immediate() == Some(Immediate::Null) =>
                    {
                        site.callee = snapshot.target;
                        continue;
                    }
                    PropertyRead::Data(trap) => {
                        return self
                            .dispatch_proxy_call_trap(site, proxy, trap, true)
                            .map(|_| ());
                    }
                    PropertyRead::Accessor(getter) => {
                        let arguments = self.create_array_argument_list_from_site(&site)?;
                        let state = self.allocate_proxy_call_state(
                            proxy,
                            arguments,
                            site.this_value,
                            site.new_target,
                        )?;
                        return self
                            .dispatch_property_callback(
                                NativeContinuation::proxy_call_getter(
                                    NativeContinuationSite {
                                        caller_base: site.caller_base,
                                        destination: site.destination,
                                        call_site: site.call_site,
                                    },
                                    Value::from_heap_ref(state.raw()),
                                    snapshot.handler,
                                    true,
                                ),
                                getter,
                            )
                            .map(|_| ());
                    }
                }
            }
            let callable = self
                .resolve_function_object(site.callee)
                .map_err(|_| ExecutionError::NonConstructor(site.callee))?;
            match callable.executable {
                FunctionExecutable::Bound(data) => {
                    let bound = self.bound_function_snapshot(data)?;
                    if site.argument_prefix.is_some() || site.argument_source.is_some() {
                        let argument_count = site
                            .argument_count
                            .checked_add(bound.argument_count)
                            .ok_or(ExecutionError::BoundArgumentCountOverflow)?;
                        let mut arguments = Vec::new();
                        arguments
                            .try_reserve_exact(argument_count as usize)
                            .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)?;
                        self.append_bound_arguments(data, &mut arguments)?;
                        for index in 0..site.argument_count {
                            arguments.push(
                                self.call_argument(&site, index)?
                                    .expect("forwarded construct argument remains in range"),
                            );
                        }
                        let prefix = self.create_apply_argument_prefix(
                            bound.call_target,
                            bound.bound_this,
                            arguments,
                        )?;
                        site.argument_count = argument_count;
                        site.argument_prefix = Some(prefix);
                        site.argument_prefix_offset = 0;
                        site.argument_prefix_count = argument_count;
                        site.argument_source = None;
                        site.argument_base = 0;
                    } else {
                        site.argument_count = site
                            .argument_count
                            .checked_add(bound.argument_count)
                            .ok_or(ExecutionError::BoundArgumentCountOverflow)?;
                        site.argument_prefix = Some(data);
                        site.argument_prefix_count = bound.argument_count;
                    }
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
                FunctionExecutable::Native(NativeFunction::StringConstructor) => {
                    return self.dispatch_conversion_native(
                        NativeFunction::StringConstructor,
                        &site,
                        true,
                    );
                }
                FunctionExecutable::Native(NativeFunction::RegExpConstructor) => {
                    let regexp = self.create_regexp_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, regexp);
                }
                FunctionExecutable::Native(NativeFunction::BooleanConstructor) => {
                    let native = NativeFunction::BooleanConstructor;
                    let value = self.primitive_constructor_value(native, &site)?;
                    let object = self.box_boolean_from_constructor(value, site.new_target)?;
                    return self.write(site.caller_base, site.destination, object);
                }
                FunctionExecutable::Native(NativeFunction::BigIntConstructor) => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::Native(NativeFunction::DateConstructor) => {
                    return self.begin_date_constructor(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectConstructor) => {
                    let object = self.create_object_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, object);
                }
                FunctionExecutable::Native(NativeFunction::ErrorConstructor(kind)) => {
                    return self.begin_error_constructor(&site, kind);
                }
                FunctionExecutable::Native(NativeFunction::ProxyConstructor) => {
                    let proxy = self.create_proxy_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, proxy);
                }
                FunctionExecutable::Native(NativeFunction::PromiseConstructor) => {
                    return self.begin_promise_constructor(&site);
                }
                FunctionExecutable::Native(NativeFunction::SignalStateConstructor) => {
                    return self.begin_signal_state_constructor(&site);
                }
                FunctionExecutable::Native(NativeFunction::SignalComputedConstructor) => {
                    return self.begin_signal_computed_constructor(&site);
                }
                FunctionExecutable::Native(NativeFunction::SignalWatcherConstructor) => {
                    let watcher = self.create_signal_watcher_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, watcher);
                }
                FunctionExecutable::Native(NativeFunction::ArrayConstructor) => {
                    let array = self.create_array_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, array);
                }
                FunctionExecutable::Native(NativeFunction::ArrayBufferConstructor) => {
                    let buffer = self.create_array_buffer_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, buffer);
                }
                FunctionExecutable::Native(NativeFunction::DataViewConstructor) => {
                    let view = self.create_data_view_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, view);
                }
                FunctionExecutable::Native(NativeFunction::TypedArrayConstructor(kind)) => {
                    return self.begin_typed_array_from_site(&site, kind);
                }
                FunctionExecutable::Native(NativeFunction::MapConstructor) => {
                    return self.begin_map_from_site(&site);
                }
                FunctionExecutable::Native(NativeFunction::SetConstructor) => {
                    return self.begin_set_from_site(&site);
                }
                FunctionExecutable::Native(NativeFunction::WeakMapConstructor) => {
                    return self.begin_weak_map_from_site(&site);
                }
                FunctionExecutable::Native(NativeFunction::WeakSetConstructor) => {
                    return self.begin_weak_set_from_site(&site);
                }
                FunctionExecutable::Native(NativeFunction::WeakRefConstructor) => {
                    let reference = self.create_weak_ref_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, reference);
                }
                FunctionExecutable::Native(NativeFunction::FinalizationRegistryConstructor) => {
                    let registry = self.create_finalization_registry_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, registry);
                }
                FunctionExecutable::Native(NativeFunction::FunctionConstructor) => {
                    let function = self.create_dynamic_function_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, function);
                }
                FunctionExecutable::Native(
                    NativeFunction::AsyncFunctionConstructor
                    | NativeFunction::GeneratorFunctionConstructor
                    | NativeFunction::AsyncGeneratorFunctionConstructor,
                ) => {
                    return Err(ExecutionError::UnsupportedDynamicFunctionConstructor);
                }
                FunctionExecutable::Bytecode { code, function, .. } => {
                    let kind = self
                        .loaded_code(code)?
                        .module
                        .function(function)
                        .ok_or(ExecutionError::MissingEntryFunction(function))?
                        .kind();
                    if matches!(
                        kind,
                        FunctionKind::ClassMethod
                            | FunctionKind::ClassFieldInitializer
                            | FunctionKind::Generator
                    ) {
                        return Err(ExecutionError::NonConstructor(site.callee));
                    }
                    break kind == FunctionKind::DerivedClassConstructor;
                }
                FunctionExecutable::ClassBytecode(data) => {
                    let data = self.class_constructor_snapshot(data)?;
                    let kind = self
                        .loaded_code(data.code)?
                        .module
                        .function(data.function)
                        .ok_or(ExecutionError::MissingEntryFunction(data.function))?
                        .kind();
                    break kind == FunctionKind::DerivedClassConstructor;
                }
                FunctionExecutable::Native(_) => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::ProxyRevoker(_) => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::PromiseResolver { .. } => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::PromiseCapabilityExecutor(_) => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::PromiseFinallyHandler { .. } => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::PromiseFinallyResultHandler { .. } => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::PromiseCombinatorHandler { .. } => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::AsyncFromSyncIteratorUnwrap { .. } => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::AsyncFromSyncIteratorCloseOnReject { .. } => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
            }
        };
        if derived {
            return self.call(site);
        }
        if self.fiber.pending_construct_sites.capacity() == 0 {
            self.fiber
                .pending_construct_sites
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::FrameAllocationFailed)?;
        }
        self.fiber.pending_construct_sites.push(site);
        let prototype_result = (|| {
            let prototype_atom = self.prototype_atom()?;
            let new_target = self
                .fiber
                .pending_construct_sites
                .last()
                .expect("construct root is active")
                .new_target;
            let prototype = self
                .constructor_prototype_value(new_target, prototype_atom)?
                .filter(|value| self.is_object_value(*value));
            if prototype.is_some() {
                return Ok(prototype);
            }
            let new_target = self
                .fiber
                .pending_construct_sites
                .last()
                .expect("construct root is active")
                .new_target;
            Ok(self.realm_for_callable(new_target).ok().and_then(|realm| {
                self.realm_intrinsic_prototype(realm, IntrinsicPrototypeKind::Object)
            }))
        })();
        site = self
            .fiber
            .pending_construct_sites
            .pop()
            .expect("construct root is balanced");
        let prototype = prototype_result?.unwrap_or(Value::from_immediate(Immediate::Null));
        let mut roots = ConstructReceiverRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
            },
            site,
        };
        let receiver = self
            .heap
            .try_allocate_with_gc(
                self.types.ordinary_object,
                0,
                0,
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|receiver| Value::from_heap_ref(receiver.raw()))
            .map_err(ExecutionError::HeapAllocation)?;
        site = roots.site;
        site.this_value = receiver;
        site.construct_receiver = Some(receiver);
        self.call(site)
    }

    /// Resolves `newTarget.prototype` through transparent Proxy layers on the constructor slow path.
    pub(crate) fn constructor_prototype_value(
        &mut self,
        mut new_target: Value,
        prototype_atom: AtomId,
    ) -> Result<Option<Value>, ExecutionError> {
        loop {
            if !self.is_proxy_value(new_target) {
                return self.get_data_property(new_target, prototype_atom);
            }
            let snapshot = self.proxy_snapshot(new_target)?;
            if snapshot.handler.as_immediate() == Some(Immediate::Null) {
                return Err(ExecutionError::ProxyRevoked);
            }
            let get_atom = self.intern_intrinsic_name(b"get")?;
            match self.resolve_property_read(snapshot.handler, get_atom.into())? {
                PropertyRead::Missing => {
                    new_target = snapshot.target;
                }
                PropertyRead::Data(value)
                    if value.as_immediate() == Some(Immediate::Undefined)
                        || value.as_immediate() == Some(Immediate::Null) =>
                {
                    new_target = snapshot.target;
                }
                PropertyRead::Data(_) | PropertyRead::Accessor(_) => {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
            }
        }
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

    /// Replaces an eligible strict frame after resolving bound forwarding, otherwise calls normally.
    #[inline(never)]
    pub(crate) fn tail_call(&mut self, mut site: CallSite) -> Result<(), ExecutionError> {
        let frame = *self
            .fiber
            .frames
            .last()
            .expect("tail call retains its caller frame");
        if frame.strictness != FunctionStrictness::Strict
            || self.call_site_is_handler_protected(frame, site.call_site)?
        {
            return self.call(site);
        }
        loop {
            match self.resolve_function_executable(site.callee)? {
                FunctionExecutable::Bound(data) => {
                    if site.argument_prefix.is_some() {
                        return self.call(site);
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
                    let target = {
                        let template = self
                            .loaded_code(code)?
                            .module
                            .function(function)
                            .ok_or(ExecutionError::MissingEntryFunction(function))?;
                        ResolvedCallTarget {
                            code,
                            function,
                            environment,
                            kind: template.kind(),
                            layout: template.layout(),
                            strictness: template.strictness(),
                        }
                    };
                    if matches!(
                        target.kind,
                        FunctionKind::DerivedClassConstructor | FunctionKind::BaseClassConstructor
                    ) || target.layout.needs_argument_source
                    {
                        return self.call(site);
                    }
                    return self.replace_tail_call_frame(target, site);
                }
                _ => return self.call(site),
            }
        }
    }

    /// Reports whether a throw or return from this call must still visit a protected region.
    #[inline]
    fn call_site_is_handler_protected(
        &self,
        frame: Frame,
        call_site: WordOffset,
    ) -> Result<bool, ExecutionError> {
        let function = self
            .loaded_code(frame.code)?
            .module
            .function(frame.function)
            .ok_or(ExecutionError::MissingEntryFunction(frame.function))?;
        Ok(function.handlers().iter().any(|handler| {
            handler.protected_start.index() <= call_site.index()
                && call_site.index() < handler.protected_end.index()
        }))
    }

    /// Reserves the target windows, copies overlapping parameters, then publishes one replacement.
    fn replace_tail_call_frame(
        &mut self,
        target: ResolvedCallTarget,
        site: CallSite,
    ) -> Result<(), ExecutionError> {
        let old = *self
            .fiber
            .frames
            .last()
            .expect("tail replacement retains one frame");
        let requested = old.base.checked_add(target.layout.register_count).ok_or(
            ExecutionError::RegisterWindowTooLarge(target.layout.register_count),
        )?;
        if requested > self.stack_limits.max_registers {
            return Err(ExecutionError::RegisterStackLimit {
                limit: self.stack_limits.max_registers,
                requested,
            });
        }
        self.reserve_tail_call_windows(target.layout, requested)?;
        if let Some(arguments) = self.fiber.argument_objects.last().copied().flatten() {
            self.detach_mapped_arguments(arguments)?;
        }
        let environment =
            if let Some(slot_count) = NonZeroU32::new(target.layout.environment_slot_count) {
                Some(
                    self.allocate_activation_environment(
                        target.kind,
                        target.environment,
                        EnvironmentOwner {
                            code: target.code,
                            function: target.function,
                        },
                        slot_count,
                        target
                            .layout
                            .self_binding_slot
                            .map(|slot| (slot, site.callee)),
                    )?,
                )
            } else {
                target.environment
            };
        let copied = site.argument_count.min(target.layout.argument_count);
        self.copy_tail_call_parameters(&site, old.base, copied)?;
        self.fiber.registers[(old.base + copied) as usize..requested as usize]
            .fill(Value::from_immediate(Immediate::Undefined));
        let this_value = self.bind_ordinary_this(target.strictness, site.this_value);
        let receiver_or_home_object = if matches!(
            target.kind,
            FunctionKind::ClassMethod | FunctionKind::ClassFieldInitializer
        ) {
            Some(self.function_home_object(site.callee)?)
        } else {
            None
        };
        let frame_depth = self.fiber.frames.len() as u32;
        while self
            .fiber
            .class_environments
            .last()
            .is_some_and(|depth| *depth >= frame_depth)
        {
            self.fiber.class_environments.pop();
        }
        while self
            .fiber
            .eval_var_environments
            .last()
            .is_some_and(|environment| environment.frame_depth >= frame_depth)
        {
            self.fiber.eval_var_environments.pop();
        }
        self.fiber.dynamic_scope =
            self.fiber.direct_eval || !self.fiber.eval_var_environments.is_empty();
        self.fiber.handlers.truncate(old.handler_base as usize);
        self.fiber
            .completions
            .truncate(old.completion_base as usize);
        self.fiber.registers.truncate(requested as usize);
        *self
            .fiber
            .argument_objects
            .last_mut()
            .expect("tail replacement retains its arguments cache") = None;
        *self
            .fiber
            .argument_sources
            .last_mut()
            .expect("tail replacement retains its argument source") = None;
        *self
            .fiber
            .argument_callees
            .last_mut()
            .expect("tail replacement retains its callee cache") = Some(site.callee);
        *self
            .fiber
            .frames
            .last_mut()
            .expect("tail replacement retains one frame") = Frame {
            code: target.code,
            function: target.function,
            pc: WordOffset::new(0),
            base: old.base,
            environment,
            return_register: old.return_register,
            return_continuation: old.return_continuation,
            this_value,
            new_target: Value::from_immediate(Immediate::Undefined),
            receiver_or_home_object,
            strictness: target.strictness,
            has_finally: target.layout.max_completion_depth != 0,
            argument_base: old.base,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: site.argument_count,
            handler_base: old.handler_base,
            completion_base: old.completion_base,
            call_site: old.call_site,
        };
        Ok(())
    }

    /// Grows only the reusable register/handler/completion windows before frame mutation.
    fn reserve_tail_call_windows(
        &mut self,
        layout: tachyon_bytecode::FunctionLayout,
        requested: u32,
    ) -> Result<(), ExecutionError> {
        let additional = requested as usize - self.fiber.registers.len().min(requested as usize);
        if additional > self.fiber.registers.capacity() - self.fiber.registers.len() {
            self.fiber
                .registers
                .try_reserve_exact(additional)
                .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        }
        let handlers = usize::try_from(layout.max_handler_depth)
            .map_err(|_| ExecutionError::HandlerStackTooLarge(layout.max_handler_depth))?;
        if handlers > self.fiber.handlers.capacity() - self.fiber.handlers.len() {
            self.fiber
                .handlers
                .try_reserve_exact(handlers)
                .map_err(|_| ExecutionError::HandlerAllocationFailed)?;
        }
        self.fiber
            .completions
            .reserve(layout.max_completion_depth as usize)
            .map_err(Self::completion_stack_error)?;
        self.fiber.registers.resize(
            self.fiber.registers.len().max(requested as usize),
            Value::from_immediate(Immediate::Undefined),
        );
        Ok(())
    }

    /// Copies formal parameters without allocating, choosing a direction safe for overlap.
    fn copy_tail_call_parameters(
        &mut self,
        site: &CallSite,
        destination: u32,
        copied: u32,
    ) -> Result<(), ExecutionError> {
        let prefix = site.argument_prefix_count.min(copied);
        let suffix_start = prefix;
        let destination_suffix = destination.saturating_add(suffix_start);
        if destination_suffix <= site.argument_base || site.argument_source.is_some() {
            for index in suffix_start..copied {
                let value = self
                    .call_argument(site, index)?
                    .expect("copied tail argument remains in range");
                self.fiber.registers[(destination + index) as usize] = value;
            }
        } else {
            for index in (suffix_start..copied).rev() {
                let value = self
                    .call_argument(site, index)?
                    .expect("copied tail argument remains in range");
                self.fiber.registers[(destination + index) as usize] = value;
            }
        }
        for index in 0..prefix {
            let value = self
                .call_argument(site, index)?
                .expect("bound tail prefix remains in range");
            self.fiber.registers[(destination + index) as usize] = value;
        }
        Ok(())
    }

    /// Resolves native forwarding iteratively, then pushes one exact bytecode frame when required.
    #[inline(never)]
    pub(crate) fn call(&mut self, mut site: CallSite) -> Result<(), ExecutionError> {
        loop {
            if self.is_proxy_value(site.callee) {
                let proxy = site.callee;
                let snapshot = self.proxy_snapshot(proxy)?;
                if snapshot.handler.as_immediate() == Some(Immediate::Null) {
                    return Err(ExecutionError::ProxyRevoked);
                }
                let apply_atom = self.intern_intrinsic_name(b"apply")?;
                match self.resolve_property_read(snapshot.handler, apply_atom.into())? {
                    PropertyRead::Missing => {
                        site.callee = snapshot.target;
                        continue;
                    }
                    PropertyRead::Data(value)
                        if value.as_immediate() == Some(Immediate::Undefined)
                            || value.as_immediate() == Some(Immediate::Null) =>
                    {
                        site.callee = snapshot.target;
                        continue;
                    }
                    PropertyRead::Data(trap) => {
                        return self
                            .dispatch_proxy_call_trap(site, proxy, trap, false)
                            .map(|_| ());
                    }
                    PropertyRead::Accessor(getter) => {
                        let arguments = self.create_array_argument_list_from_site(&site)?;
                        let state = self.allocate_proxy_call_state(
                            proxy,
                            arguments,
                            site.this_value,
                            site.new_target,
                        )?;
                        return self
                            .dispatch_property_callback(
                                NativeContinuation::proxy_call_getter(
                                    NativeContinuationSite {
                                        caller_base: site.caller_base,
                                        destination: site.destination,
                                        call_site: site.call_site,
                                    },
                                    Value::from_heap_ref(state.raw()),
                                    snapshot.handler,
                                    false,
                                ),
                                getter,
                            )
                            .map(|_| ());
                    }
                }
            }
            match self.resolve_function_executable(site.callee)? {
                FunctionExecutable::Bound(data) => {
                    let bound = self.bound_function_snapshot(data)?;
                    if site.argument_prefix.is_some() || site.argument_source.is_some() {
                        let argument_count = site
                            .argument_count
                            .checked_add(bound.argument_count)
                            .ok_or(ExecutionError::BoundArgumentCountOverflow)?;
                        let mut arguments = Vec::new();
                        arguments
                            .try_reserve_exact(argument_count as usize)
                            .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)?;
                        self.append_bound_arguments(data, &mut arguments)?;
                        for index in 0..site.argument_count {
                            arguments.push(
                                self.call_argument(&site, index)?
                                    .expect("forwarded call argument remains in range"),
                            );
                        }
                        let prefix = self.create_apply_argument_prefix(
                            bound.call_target,
                            bound.bound_this,
                            arguments,
                        )?;
                        site.argument_count = argument_count;
                        site.argument_prefix = Some(prefix);
                        site.argument_prefix_offset = 0;
                        site.argument_prefix_count = argument_count;
                        site.argument_source = None;
                        site.argument_base = 0;
                    } else {
                        site.argument_count = site
                            .argument_count
                            .checked_add(bound.argument_count)
                            .ok_or(ExecutionError::BoundArgumentCountOverflow)?;
                        site.argument_prefix = Some(data);
                        site.argument_prefix_count = bound.argument_count;
                    }
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
                    if matches!(
                        kind,
                        FunctionKind::DerivedClassConstructor | FunctionKind::BaseClassConstructor
                    ) && site.new_target.as_immediate() == Some(Immediate::Undefined)
                    {
                        return Err(ExecutionError::ClassConstructorCalledWithoutNew(
                            site.callee,
                        ));
                    }
                    if matches!(kind, FunctionKind::Generator | FunctionKind::AsyncGenerator) {
                        if site.new_target.as_immediate() != Some(Immediate::Undefined) {
                            return Err(ExecutionError::NonConstructor(site.callee));
                        }
                        let generator = self.create_generator_from_site(
                            &site,
                            ResolvedCallTarget {
                                code,
                                function,
                                environment,
                                kind,
                                layout,
                                strictness,
                            },
                        )?;
                        return self.write(site.caller_base, site.destination, generator);
                    }
                    if kind == FunctionKind::Async {
                        if site.new_target.as_immediate() != Some(Immediate::Undefined) {
                            return Err(ExecutionError::NonConstructor(site.callee));
                        }
                        return self.begin_async_function(
                            &site,
                            ResolvedCallTarget {
                                code,
                                function,
                                environment,
                                kind,
                                layout,
                                strictness,
                            },
                        );
                    }
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
                FunctionExecutable::ClassBytecode(data) => {
                    let data = self.class_constructor_snapshot(data)?;
                    let (kind, layout, strictness) = {
                        let function_template = self
                            .loaded_code(data.code)?
                            .module
                            .function(data.function)
                            .ok_or(ExecutionError::MissingEntryFunction(data.function))?;
                        (
                            function_template.kind(),
                            function_template.layout(),
                            function_template.strictness(),
                        )
                    };
                    if site.new_target.as_immediate() == Some(Immediate::Undefined) {
                        return Err(ExecutionError::ClassConstructorCalledWithoutNew(
                            site.callee,
                        ));
                    }
                    return self.push_call_frame(
                        ResolvedCallTarget {
                            code: data.code,
                            function: data.function,
                            environment: data.environment,
                            kind,
                            layout,
                            strictness,
                        },
                        site,
                    );
                }
                FunctionExecutable::ProxyRevoker(_) => {
                    self.revoke_proxy_from_function(site.callee)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(Immediate::Undefined),
                    );
                }
                FunctionExecutable::PromiseResolver { cell, reject } => {
                    let resolution = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    return self.begin_promise_resolver_call(&site, cell, reject, resolution);
                }
                FunctionExecutable::PromiseCapabilityExecutor(capability) => {
                    return self.call_promise_capability_executor(&site, capability);
                }
                FunctionExecutable::PromiseFinallyHandler { state, .. } => {
                    let callback = self.native_call_state_snapshot(state)?.values[0];
                    let original = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let continuation_site = NativeContinuationSite {
                        caller_base: site.caller_base,
                        destination: site.destination,
                        call_site: site.call_site,
                    };
                    let completion_depth = self.fiber.completions.len();
                    self.fiber
                        .completions
                        .push_native(NativeContinuation::promise_finally(
                            continuation_site,
                            site.callee,
                            original,
                        ))
                        .map_err(Isolate::completion_stack_error)?;
                    let frame_depth = self.fiber.frames.len();
                    if let Err(error) = self.call(CallSite {
                        caller_base: site.caller_base,
                        destination: site.destination,
                        callee: callback,
                        argument_base: 0,
                        argument_source: None,
                        argument_prefix: None,
                        argument_prefix_offset: 0,
                        argument_prefix_count: 0,
                        argument_count: 0,
                        this_value: Value::from_immediate(Immediate::Undefined),
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
                            .expect("finally callback publishes one frame");
                        frame.return_register = None;
                        frame.return_continuation = true;
                        return Ok(());
                    }
                    if self.fiber.completions.len() > completion_depth {
                        let continuation = self.pop_native_continuation()?;
                        let callback_result = self.read(site.caller_base, site.destination)?;
                        return self.finish_promise_finally_callback(continuation, callback_result);
                    }
                    return self.write(site.caller_base, site.destination, original);
                }
                FunctionExecutable::PromiseFinallyResultHandler { state, rejected } => {
                    let value = self.native_call_state_snapshot(state)?.values[0];
                    if rejected {
                        return Err(ExecutionError::HostThrown(value));
                    }
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::PromiseCombinatorHandler { element, rejected } => {
                    return self.call_promise_all_handler(&site, element, rejected);
                }
                FunctionExecutable::AsyncFromSyncIteratorUnwrap { done } => {
                    let value = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let result = self.create_iterator_result(value, done)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::AsyncFromSyncIteratorCloseOnReject { iterator } => {
                    let reason = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    return self.begin_async_from_sync_close_on_reject(&site, iterator, reason);
                }
                FunctionExecutable::Native(NativeFunction::FunctionPrototype) => {
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(Immediate::Undefined),
                    );
                }
                FunctionExecutable::Native(NativeFunction::HostCreateRealm) => {
                    let (_, global) = self.create_realm()?;
                    let result = self.create_ordinary_object()?;
                    let global_atom = self.intern_intrinsic_name(b"global")?;
                    self.set_own_data_property(result, global_atom, global)?;
                    let eval_atom = self.intern_intrinsic_name(b"eval")?;
                    let eval_script = self
                        .get_data_property(global, eval_atom)?
                        .ok_or(ExecutionError::UnsupportedDynamicFunctionConstructor)?;
                    let eval_script_atom = self.intern_intrinsic_name(b"evalScript")?;
                    self.set_own_data_property(result, eval_script_atom, eval_script)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::HostEvalScript) => {
                    let source = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    if !self.is_string_value(source) {
                        return self.write(site.caller_base, site.destination, source);
                    }
                    let callback = self
                        .eval_script_callback
                        .ok_or(ExecutionError::UnsupportedDynamicFunctionConstructor)?;
                    let realm = self.realm_for_callable(site.callee)?;
                    let result = callback(self, realm, EvalKind::Indirect, source)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::HostDetachArrayBuffer) => {
                    let buffer = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    self.detach_array_buffer(buffer)?;
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
                FunctionExecutable::Native(NativeFunction::StringCharAt) => {
                    let value = self.string_char_at(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringCharCodeAt) => {
                    let value = self.string_char_code_at(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringAt) => {
                    let value = self.string_at(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringCodePointAt) => {
                    let value = self.string_code_point_at(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringFromCharCode) => {
                    let value = self.string_from_char_code(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringFromCodePoint) => {
                    let value = self.string_from_code_point(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(
                    NativeFunction::StringToString | NativeFunction::StringValueOf,
                ) => {
                    let value = self.string_primitive_value(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringIsWellFormed) => {
                    let value = self.string_is_well_formed(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringToWellFormed) => {
                    let value = self.string_to_well_formed(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringSlice) => {
                    let value = self.string_slice(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringSubstring) => {
                    let value = self.string_substring(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringIndexOf) => {
                    let value = self.string_index_of(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringIncludes) => {
                    let value = self.string_includes(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringLastIndexOf) => {
                    let value = self.string_last_index_of(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringStartsWith) => {
                    let value = self.string_starts_with(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringEndsWith) => {
                    let value = self.string_ends_with(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringConcat) => {
                    let value = self.string_concat(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringRepeat) => {
                    let value = self.string_repeat(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringPadStart) => {
                    let value = self.string_pad(&site, false)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringPadEnd) => {
                    let value = self.string_pad(&site, true)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::StringSplit) => {
                    return self.begin_string_split(&site);
                }
                FunctionExecutable::Native(NativeFunction::StringTrim) => {
                    return self
                        .dispatch_string_receiver_conversion(NativeFunction::StringTrim, &site);
                }
                FunctionExecutable::Native(NativeFunction::StringTrimStart) => {
                    return self.dispatch_string_receiver_conversion(
                        NativeFunction::StringTrimStart,
                        &site,
                    );
                }
                FunctionExecutable::Native(NativeFunction::StringTrimEnd) => {
                    return self
                        .dispatch_string_receiver_conversion(NativeFunction::StringTrimEnd, &site);
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::StringToLowerCase
                    | NativeFunction::StringToUpperCase
                    | NativeFunction::StringToLocaleLowerCase
                    | NativeFunction::StringToLocaleUpperCase),
                ) => return self.dispatch_string_receiver_conversion(native, &site),
                FunctionExecutable::Native(NativeFunction::StringIterator) => {
                    return self.dispatch_string_receiver_conversion(
                        NativeFunction::StringIterator,
                        &site,
                    );
                }
                FunctionExecutable::Native(NativeFunction::StringIteratorNext) => {
                    let value = self.string_iterator_next(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::StringConstructor
                    | NativeFunction::NumberToExponential
                    | NativeFunction::NumberToFixed
                    | NativeFunction::NumberToPrecision
                    | NativeFunction::NumberToString
                    | NativeFunction::NumberConstructor
                    | NativeFunction::BigIntConstructor
                    | NativeFunction::BigIntAsIntN
                    | NativeFunction::BigIntAsUintN
                    | NativeFunction::BigIntToString
                    | NativeFunction::DateParse),
                ) => return self.dispatch_conversion_native(native, &site, false),
                FunctionExecutable::Native(NativeFunction::BigIntValueOf) => {
                    let value = self.this_bigint_value(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::BigIntToLocaleString) => {
                    let value = self.bigint_to_string(site.this_value, None)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::NumberToLocaleString) => {
                    let receiver = self.this_number_value(site.this_value)?;
                    let value = self.number_to_string(receiver, None)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::RegExpConstructor) => {
                    let regexp = self.create_regexp_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, regexp);
                }
                FunctionExecutable::Native(NativeFunction::RegExpSplit) => {
                    let result = self.regexp_split(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::StringSearch) => {
                    return self.begin_string_search(&site);
                }
                FunctionExecutable::Native(NativeFunction::StringMatch) => {
                    let result = self.string_match(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::StringMatchAll) => {
                    let result = self.string_match_all(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::StringReplace) => {
                    return self.string_replace(&site);
                }
                FunctionExecutable::Native(NativeFunction::StringReplaceAll) => {
                    return self.begin_string_replace_all(&site);
                }
                FunctionExecutable::Native(NativeFunction::RegExpEscape) => {
                    let result = self.regexp_escape(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::DateConstructor) => {
                    let value = self.date_function_call()?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::DateNow) => {
                    let value = self.date_now()?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(
                    NativeFunction::DateGetTime | NativeFunction::DateValueOf,
                ) => {
                    let value = self.date_prototype_time_value(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::DateUtc) => {
                    return self.begin_date_utc(&site);
                }
                FunctionExecutable::Native(NativeFunction::DateSetTime) => {
                    return self.begin_date_set_time(&site);
                }
                FunctionExecutable::Native(NativeFunction::DateToISOString) => {
                    let value = self.date_to_iso_string(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::DateToUtcString) => {
                    let value = self.date_to_utc_string(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(
                    NativeFunction::DateToString | NativeFunction::DateToLocaleString,
                ) => {
                    let value = self.date_to_string(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(
                    NativeFunction::DateToDateString | NativeFunction::DateToLocaleDateString,
                ) => {
                    let value = self.date_to_date_string(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(
                    NativeFunction::DateToTimeString | NativeFunction::DateToLocaleTimeString,
                ) => {
                    let value = self.date_to_time_string(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::DateToPrimitive) => {
                    return self.begin_date_to_primitive(&site);
                }
                FunctionExecutable::Native(NativeFunction::DateToJson) => {
                    return self.begin_date_to_json(&site);
                }
                FunctionExecutable::Native(NativeFunction::DateUtcGetter(field)) => {
                    let value = self.date_utc_field_value(site.this_value, field)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::DateLocalGetter(field)) => {
                    let value = self.date_local_field_value(site.this_value, field)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::DateTimezoneOffset) => {
                    let value = self.date_timezone_offset(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::DateUtcSetter(setter)) => {
                    return self.begin_date_utc_setter(&site, setter);
                }
                FunctionExecutable::Native(NativeFunction::DateLocalSetter(setter)) => {
                    return self.begin_date_local_setter(&site, setter);
                }
                FunctionExecutable::Native(NativeFunction::RegExpExec) => {
                    return self.begin_regexp_exec(&site);
                }
                FunctionExecutable::Native(NativeFunction::RegExpTest) => {
                    return self.begin_regexp_test(&site);
                }
                FunctionExecutable::Native(NativeFunction::RegExpSearch) => {
                    return self.begin_regexp_search(&site);
                }
                FunctionExecutable::Native(NativeFunction::RegExpMatch) => {
                    let result = self.regexp_match(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::RegExpMatchAll) => {
                    let result = self.regexp_match_all(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::RegExpStringIteratorNext) => {
                    return self.regexp_string_iterator_next(&site);
                }
                FunctionExecutable::Native(NativeFunction::RegExpReplace) => {
                    return self.regexp_replace(&site);
                }
                FunctionExecutable::Native(NativeFunction::RegExpToString) => {
                    let result = self.regexp_to_string(site.this_value)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::RegExpGetter(getter)) => {
                    if getter == RegExpGetter::Flags {
                        return self.begin_regexp_flags(&site);
                    }
                    let getter_realm = self.realm_for_callable(site.callee)?;
                    let result = self.regexp_getter(site.this_value, getter, getter_realm)?;
                    return self.write(site.caller_base, site.destination, result);
                }
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
                FunctionExecutable::Native(NativeFunction::SymbolFor) => {
                    let value = self.symbol_for(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::SymbolKeyFor) => {
                    let value = self.symbol_key_for(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::SymbolToString) => {
                    let value = self.symbol_to_string(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::SymbolValueOf) => {
                    let value = self.symbol_value_of(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::BooleanToString) => {
                    let value = self.boolean_to_string(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::BooleanValueOf) => {
                    let value = self.this_boolean_value(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::SymbolDescription) => {
                    let value = self.symbol_description_get(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::SymbolToPrimitive) => {
                    let value = self.symbol_to_primitive(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ObjectDefineProperty) => {
                    return self.object_define_property(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectDefineProperties) => {
                    return self.begin_object_define_properties(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectFromEntries) => {
                    return self.begin_object_from_entries(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectGroupBy) => {
                    return self.begin_object_group_by(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectGetOwnPropertyDescriptor) => {
                    return self.object_get_own_property_descriptor(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectGetOwnPropertyDescriptors) => {
                    return self.begin_object_get_own_property_descriptors(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectGetOwnPropertyNames) => {
                    if self.try_dispatch_proxy_own_keys(&site, ProxyOwnKeysMode::Names)? {
                        return Ok(());
                    }
                    let result = self.object_get_own_property_names(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::ObjectGetOwnPropertySymbols) => {
                    if self.try_dispatch_proxy_own_keys(&site, ProxyOwnKeysMode::Symbols)? {
                        return Ok(());
                    }
                    let result = self.object_get_own_property_symbols(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::ObjectHasOwnProperty) => {
                    return self.object_has_own_property(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectPropertyIsEnumerable) => {
                    return self.object_property_is_enumerable(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectDefineGetter) => {
                    return self.object_define_legacy_accessor(&site, false);
                }
                FunctionExecutable::Native(NativeFunction::ObjectDefineSetter) => {
                    return self.object_define_legacy_accessor(&site, true);
                }
                FunctionExecutable::Native(NativeFunction::ObjectLookupGetter) => {
                    return self.object_lookup_legacy_accessor(&site, false);
                }
                FunctionExecutable::Native(NativeFunction::ObjectLookupSetter) => {
                    return self.object_lookup_legacy_accessor(&site, true);
                }
                FunctionExecutable::Native(NativeFunction::ObjectProtoGetter) => {
                    return self.object_proto_getter(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectProtoSetter) => {
                    return self.object_proto_setter(&site);
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
                    if self
                        .try_dispatch_proxy_builtin(&site, ProxyInternalMethod::GetPrototypeOf)?
                    {
                        return Ok(());
                    }
                    let prototype = self.object_get_prototype_of(&site)?;
                    return self.write(site.caller_base, site.destination, prototype);
                }
                FunctionExecutable::Native(NativeFunction::ObjectCreate) => {
                    return self.begin_object_create(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectIsPrototypeOf) => {
                    return self.begin_object_is_prototype_of(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectSetPrototypeOf) => {
                    if self.dispatch_proxy_set_prototype_from_site(
                        &site,
                        ProxySetPrototypeMode::Object,
                    )? {
                        return Ok(());
                    }
                    let value = self.object_set_prototype_of(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ObjectIsExtensible) => {
                    if self.try_dispatch_proxy_builtin(&site, ProxyInternalMethod::IsExtensible)? {
                        return Ok(());
                    }
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
                    if self.try_dispatch_proxy_builtin(
                        &site,
                        ProxyInternalMethod::PreventExtensionsObject,
                    )? {
                        return Ok(());
                    }
                    let value = self.object_prevent_extensions(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ObjectSeal) => {
                    let value = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let value = self.object_set_integrity_level(value, false)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ObjectFreeze) => {
                    let value = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let value = self.object_set_integrity_level(value, true)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ObjectIsSealed) => {
                    let value = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    if self.is_proxy_value(value) {
                        return self
                            .begin_proxy_test_integrity(
                                NativeContinuationSite {
                                    caller_base: site.caller_base,
                                    destination: site.destination,
                                    call_site: site.call_site,
                                },
                                value,
                                false,
                            )
                            .map(|_| ());
                    }
                    let result = self.object_test_integrity_level(value, false)?;
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
                FunctionExecutable::Native(NativeFunction::ObjectIsFrozen) => {
                    let value = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    if self.is_proxy_value(value) {
                        return self
                            .begin_proxy_test_integrity(
                                NativeContinuationSite {
                                    caller_base: site.caller_base,
                                    destination: site.destination,
                                    call_site: site.call_site,
                                },
                                value,
                                true,
                            )
                            .map(|_| ());
                    }
                    let result = self.object_test_integrity_level(value, true)?;
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
                FunctionExecutable::Native(NativeFunction::ReflectOwnKeys) => {
                    if self.try_dispatch_proxy_own_keys(&site, ProxyOwnKeysMode::Reflect)? {
                        return Ok(());
                    }
                    let keys = self.reflect_own_keys(&site)?;
                    return self.write(site.caller_base, site.destination, keys);
                }
                FunctionExecutable::Native(NativeFunction::ReflectDefineProperty) => {
                    return self.reflect_define_property(&site);
                }
                FunctionExecutable::Native(NativeFunction::ReflectDeleteProperty) => {
                    return self.reflect_delete_property(&site);
                }
                FunctionExecutable::Native(NativeFunction::ReflectGetOwnPropertyDescriptor) => {
                    return self.reflect_get_own_property_descriptor(&site);
                }
                FunctionExecutable::Native(NativeFunction::ReflectGet) => {
                    return self.reflect_get(&site);
                }
                FunctionExecutable::Native(NativeFunction::ReflectGetPrototypeOf) => {
                    if self
                        .try_dispatch_proxy_builtin(&site, ProxyInternalMethod::GetPrototypeOf)?
                    {
                        return Ok(());
                    }
                    let prototype = self.reflect_get_prototype_of(&site)?;
                    return self.write(site.caller_base, site.destination, prototype);
                }
                FunctionExecutable::Native(NativeFunction::ReflectHas) => {
                    return self.reflect_has(&site);
                }
                FunctionExecutable::Native(NativeFunction::ReflectSet) => {
                    return self.reflect_set(&site);
                }
                FunctionExecutable::Native(NativeFunction::ReflectSetPrototypeOf) => {
                    if self.dispatch_proxy_set_prototype_from_site(
                        &site,
                        ProxySetPrototypeMode::Reflect,
                    )? {
                        return Ok(());
                    }
                    let result = self.reflect_set_prototype_of(&site)?;
                    return self.write(site.caller_base, site.destination, boolean_value(result));
                }
                FunctionExecutable::Native(NativeFunction::ReflectIsExtensible) => {
                    if self.try_dispatch_proxy_builtin(&site, ProxyInternalMethod::IsExtensible)? {
                        return Ok(());
                    }
                    let result = self.reflect_is_extensible(&site)?;
                    return self.write(site.caller_base, site.destination, boolean_value(result));
                }
                FunctionExecutable::Native(NativeFunction::ReflectPreventExtensions) => {
                    if self
                        .try_dispatch_proxy_builtin(&site, ProxyInternalMethod::PreventExtensions)?
                    {
                        return Ok(());
                    }
                    let result = self.reflect_prevent_extensions(&site)?;
                    return self.write(site.caller_base, site.destination, boolean_value(result));
                }
                FunctionExecutable::Native(NativeFunction::ObjectToString) => {
                    return self.begin_object_to_string(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectToLocaleString) => {
                    return self.begin_object_to_locale_string(&site);
                }
                FunctionExecutable::Native(NativeFunction::ObjectValueOf) => {
                    let value = self.object_value_of(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ObjectAssign) => {
                    return self.begin_object_assign(&site);
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::ObjectKeys
                    | NativeFunction::ObjectValues
                    | NativeFunction::ObjectEntries),
                ) => {
                    if native == NativeFunction::ObjectKeys
                        && self
                            .try_dispatch_proxy_own_keys(&site, ProxyOwnKeysMode::EnumerableNames)?
                    {
                        return Ok(());
                    }
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
                FunctionExecutable::Native(NativeFunction::FunctionPrototypeApply) => {
                    let this_value = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let argument_list = self.call_argument(&site, 1)?;
                    let Some(argument_list) = argument_list else {
                        site.callee = site.this_value;
                        site.this_value = this_value;
                        site.argument_base = 0;
                        site.argument_prefix = None;
                        site.argument_prefix_offset = 0;
                        site.argument_prefix_count = 0;
                        site.argument_count = 0;
                        continue;
                    };
                    if !self.is_object_value(argument_list) {
                        return Err(ExecutionError::NotObject(argument_list));
                    }
                    self.resolve_function_object(site.this_value)?;
                    return self.begin_argument_list(
                        &site,
                        argument_list,
                        site.this_value,
                        this_value,
                        Value::from_immediate(Immediate::Undefined),
                        ArgumentListOperation::FunctionApply,
                    );
                }
                FunctionExecutable::Native(NativeFunction::ReflectApply) => {
                    let target = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let this_value = self
                        .call_argument(&site, 1)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let argument_list =
                        self.call_argument(&site, 2)?
                            .ok_or(ExecutionError::NotObject(Value::from_immediate(
                                Immediate::Undefined,
                            )))?;
                    if !self.is_object_value(argument_list) {
                        return Err(ExecutionError::NotObject(argument_list));
                    }
                    if !self.is_callable_value(target)? {
                        return Err(ExecutionError::NonCallable(target));
                    }
                    return self.begin_argument_list(
                        &site,
                        argument_list,
                        target,
                        this_value,
                        Value::from_immediate(Immediate::Undefined),
                        ArgumentListOperation::ReflectApply,
                    );
                }
                FunctionExecutable::Native(NativeFunction::ReflectConstruct) => {
                    let target = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let arguments = self
                        .call_argument(&site, 1)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let new_target = self.call_argument(&site, 2)?.unwrap_or(target);
                    if !self.is_object_value(arguments) {
                        return Err(ExecutionError::NotObject(arguments));
                    }
                    if !self.is_constructor_value(new_target)? {
                        return Err(ExecutionError::NonConstructor(new_target));
                    }
                    if !self.is_constructor_value(target)? {
                        return Err(ExecutionError::NonConstructor(target));
                    }
                    return self.begin_argument_list(
                        &site,
                        arguments,
                        target,
                        Value::from_immediate(Immediate::Undefined),
                        new_target,
                        ArgumentListOperation::ReflectConstruct,
                    );
                }
                FunctionExecutable::Native(NativeFunction::FunctionPrototypeBind) => {
                    let bound = self.create_bound_function(&site)?;
                    return self.write(site.caller_base, site.destination, bound);
                }
                FunctionExecutable::Native(NativeFunction::FunctionConstructor) => {
                    let function = self.create_dynamic_function_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, function);
                }
                FunctionExecutable::Native(
                    NativeFunction::AsyncFunctionConstructor
                    | NativeFunction::GeneratorFunctionConstructor
                    | NativeFunction::AsyncGeneratorFunctionConstructor,
                ) => {
                    return Err(ExecutionError::UnsupportedDynamicFunctionConstructor);
                }
                FunctionExecutable::Native(NativeFunction::ErrorConstructor(kind)) => {
                    return self.begin_error_constructor(&site, kind);
                }
                FunctionExecutable::Native(NativeFunction::ErrorIsError) => {
                    let value = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let result = self.native_error_kind(value)?.is_some();
                    return self.write(site.caller_base, site.destination, boolean_value(result));
                }
                FunctionExecutable::Native(NativeFunction::ErrorToString) => {
                    return self.begin_error_to_string(&site);
                }
                FunctionExecutable::Native(NativeFunction::ErrorStackGetter) => {
                    let result = self.error_stack_getter(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::ErrorStackSetter) => {
                    return self.begin_error_stack_setter(&site);
                }
                FunctionExecutable::Native(NativeFunction::ProxyConstructor) => {
                    return Err(ExecutionError::ProxyConstructorRequiresNew);
                }
                FunctionExecutable::Native(NativeFunction::PromiseConstructor) => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::Native(NativeFunction::ProxyRevocable) => {
                    let result = self.create_revocable_proxy_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::PromiseResolve) => {
                    let value = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let intrinsic = self.realm.promise_constructor.expect("initialized");
                    if site.this_value != intrinsic {
                        return self.begin_generic_promise_resolve(&site, site.this_value, value);
                    }
                    if let Ok(snapshot) = self.promise_snapshot(value) {
                        let intrinsic_promise = matches!(
                            snapshot.state,
                            PromiseState::Pending
                                | PromiseState::Fulfilled
                                | PromiseState::Rejected
                        );
                        if intrinsic_promise && site.this_value == intrinsic {
                            // An own data `constructor` override suppresses the identity fast path.
                            let (_, ordinary) = self.object_snapshot(value)?;
                            let constructor_atom = self.constructor_atom()?;
                            let identity_fast_path =
                                match self.shapes.lookup(ordinary.shape, constructor_atom) {
                                    None => true,
                                    Some(property) if property.kind == PropertyKind::Data => {
                                        self.property_value_from_snapshot(ordinary, property)?
                                            == Some(intrinsic)
                                    }
                                    Some(_) => false,
                                };
                            if identity_fast_path {
                                return self.write(site.caller_base, site.destination, value);
                            }
                        }
                    }
                    let promise = self.create_promise(
                        PromiseState::Pending,
                        Value::from_immediate(Immediate::Undefined),
                    )?;
                    self.write(site.caller_base, site.destination, promise)?;
                    return self.begin_promise_resolution(
                        promise,
                        value,
                        NativeContinuationSite {
                            caller_base: site.caller_base,
                            destination: site.destination,
                            call_site: site.call_site,
                        },
                        PromiseResolutionMode::StaticResolve,
                    );
                }
                FunctionExecutable::Native(NativeFunction::PromiseReject) => {
                    let reason = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let intrinsic = self.realm.promise_constructor.expect("initialized");
                    if site.this_value != intrinsic {
                        return self.begin_generic_promise_reject(&site, site.this_value, reason);
                    }
                    let promise = self.create_promise(PromiseState::Rejected, reason)?;
                    return self.write(site.caller_base, site.destination, promise);
                }
                FunctionExecutable::Native(NativeFunction::PromiseTry) => {
                    return self.begin_promise_try(&site);
                }
                FunctionExecutable::Native(NativeFunction::PromiseWithResolvers) => {
                    let result = self.promise_with_resolvers(site.this_value)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::PromiseAll) => {
                    return self.begin_promise_all(&site);
                }
                FunctionExecutable::Native(NativeFunction::PromiseAllSettled) => {
                    return self.begin_promise_all_settled(&site);
                }
                FunctionExecutable::Native(NativeFunction::PromiseAny) => {
                    return self.begin_promise_any(&site);
                }
                FunctionExecutable::Native(NativeFunction::PromiseRace) => {
                    return self.begin_promise_race(&site);
                }
                FunctionExecutable::Native(NativeFunction::SpeciesGetter) => {
                    return self.write(site.caller_base, site.destination, site.this_value);
                }
                FunctionExecutable::Native(NativeFunction::PromiseThen) => {
                    return self.begin_promise_then(&site);
                }
                FunctionExecutable::Native(NativeFunction::PromiseCatch) => {
                    return self.promise_catch(&site);
                }
                FunctionExecutable::Native(NativeFunction::PromiseFinally) => {
                    return self.promise_finally(&site);
                }
                FunctionExecutable::Native(
                    NativeFunction::SignalStateConstructor
                    | NativeFunction::SignalComputedConstructor
                    | NativeFunction::SignalWatcherConstructor,
                ) => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::Native(NativeFunction::SignalStateGet) => {
                    let value = self.signal_state_get(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::SignalStateSet) => {
                    return self.begin_signal_state_set(&site);
                }
                FunctionExecutable::Native(NativeFunction::SignalComputedGet) => {
                    return self.begin_signal_computed_get(&site);
                }
                FunctionExecutable::Native(NativeFunction::SignalWatcherWatch) => {
                    return self.signal_watcher_watch(&site);
                }
                FunctionExecutable::Native(NativeFunction::SignalWatcherUnwatch) => {
                    return self.signal_watcher_unwatch(&site);
                }
                FunctionExecutable::Native(NativeFunction::SignalWatcherGetPending) => {
                    let result = self.signal_watcher_get_pending(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::SignalUntrack) => {
                    return self.begin_signal_untrack(&site);
                }
                FunctionExecutable::Native(NativeFunction::GeneratorNext) => {
                    return self.begin_generator_next(&site);
                }
                FunctionExecutable::Native(NativeFunction::GeneratorReturn) => {
                    return self.begin_generator_return(&site);
                }
                FunctionExecutable::Native(NativeFunction::GeneratorThrow) => {
                    return self.begin_generator_throw(&site);
                }
                FunctionExecutable::Native(NativeFunction::AsyncGeneratorNext) => {
                    return self.begin_async_generator_next(&site);
                }
                FunctionExecutable::Native(NativeFunction::AsyncGeneratorReturn) => {
                    return self.begin_async_generator_return(&site);
                }
                FunctionExecutable::Native(NativeFunction::AsyncGeneratorThrow) => {
                    return self.begin_async_generator_throw(&site);
                }
                FunctionExecutable::Native(NativeFunction::AsyncFromSyncIteratorNext) => {
                    return self.begin_async_from_sync_iterator_next(&site);
                }
                FunctionExecutable::Native(NativeFunction::AsyncFromSyncIteratorReturn) => {
                    return self.begin_async_from_sync_iterator_return(&site);
                }
                FunctionExecutable::Native(NativeFunction::AsyncFromSyncIteratorThrow) => {
                    return self.begin_async_from_sync_iterator_throw(&site);
                }
                FunctionExecutable::Native(NativeFunction::GeneratorFunctionPrototype) => {
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(Immediate::Undefined),
                    );
                }
                FunctionExecutable::Native(NativeFunction::AsyncFunctionPrototype) => {
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(Immediate::Undefined),
                    );
                }
                FunctionExecutable::Native(NativeFunction::AsyncGeneratorFunctionPrototype) => {
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(Immediate::Undefined),
                    );
                }
                FunctionExecutable::Native(NativeFunction::SignalCurrentComputed) => {
                    let result = self.signal_current_computed();
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::SignalIntrospectSources) => {
                    return self.signal_introspect_sources(&site);
                }
                FunctionExecutable::Native(NativeFunction::SignalIntrospectSinks) => {
                    return self.signal_introspect_sinks(&site);
                }
                FunctionExecutable::Native(NativeFunction::SignalHasSources) => {
                    let result = self.signal_has_sources(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::SignalHasSinks) => {
                    let result = self.signal_has_sinks(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::SignalIsState
                    | NativeFunction::SignalIsComputed
                    | NativeFunction::SignalIsWatcher),
                ) => {
                    let value = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let result = match native {
                        NativeFunction::SignalIsState => self.signal_is_state(value),
                        NativeFunction::SignalIsComputed => self.signal_is_computed(value),
                        NativeFunction::SignalIsWatcher => self.signal_is_watcher(value),
                        _ => unreachable!("Signal guard dispatch only binds guard functions"),
                    };
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::ArrayConstructor) => {
                    let array = self.create_array_from_site(&site)?;
                    return self.write(site.caller_base, site.destination, array);
                }
                FunctionExecutable::Native(NativeFunction::ArrayBufferConstructor) => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::Native(NativeFunction::DataViewConstructor) => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::Native(
                    NativeFunction::TypedArrayBaseConstructor
                    | NativeFunction::TypedArrayConstructor(_),
                ) => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::Native(NativeFunction::ArrayBufferIsView) => {
                    let value = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let result = self.array_buffer_is_view(value);
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::ArrayBufferSlice) => {
                    return self.begin_array_buffer_slice(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayBufferTransfer) => {
                    return self.begin_array_buffer_transfer(&site, false);
                }
                FunctionExecutable::Native(NativeFunction::ArrayBufferTransferToFixedLength) => {
                    return self.begin_array_buffer_transfer(&site, true);
                }
                FunctionExecutable::Native(NativeFunction::ArrayBufferResize) => {
                    let argument = self
                        .call_argument(&site, 0)?
                        .unwrap_or(Value::from_immediate(Immediate::Undefined));
                    let value = self.resize_array_buffer(site.this_value, argument)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::ArrayBufferByteLength
                    | NativeFunction::ArrayBufferMaxByteLength
                    | NativeFunction::ArrayBufferResizable
                    | NativeFunction::ArrayBufferDetached),
                ) => {
                    let value = self.array_buffer_getter(site.this_value, native)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::DataViewBuffer
                    | NativeFunction::DataViewByteLength
                    | NativeFunction::DataViewByteOffset),
                ) => {
                    let value = self.data_view_getter(site.this_value, native)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::DataViewGet(element)) => {
                    let value = self.data_view_get(&site, element)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::DataViewSet(element)) => {
                    let value = self.data_view_set(&site, element)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::TypedArrayGetter(getter)) => {
                    let value = self.typed_array_getter(site.this_value, getter)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::TypedArrayAt) => {
                    return self.begin_typed_array_at(&site);
                }
                FunctionExecutable::Native(NativeFunction::TypedArrayIncludes) => {
                    return self.begin_typed_array_includes(&site);
                }
                FunctionExecutable::Native(NativeFunction::TypedArrayFill) => {
                    return self.begin_typed_array_fill(&site);
                }
                FunctionExecutable::Native(NativeFunction::TypedArrayCopyWithin) => {
                    return self.begin_typed_array_copy_within(&site);
                }
                FunctionExecutable::Native(NativeFunction::TypedArrayReverse) => {
                    return self.begin_typed_array_reverse(&site);
                }
                FunctionExecutable::Native(NativeFunction::TypedArrayToReversed) => {
                    return self.begin_typed_array_to_reversed(&site);
                }
                FunctionExecutable::Native(NativeFunction::TypedArraySort) => {
                    return self.begin_typed_array_sort(&site);
                }
                FunctionExecutable::Native(NativeFunction::TypedArrayToSorted) => {
                    return self.begin_typed_array_to_sorted(&site);
                }
                FunctionExecutable::Native(NativeFunction::TypedArrayWith) => {
                    return self.begin_typed_array_with(&site);
                }
                FunctionExecutable::Native(NativeFunction::TypedArraySet) => {
                    return self.begin_typed_array_set(&site);
                }
                FunctionExecutable::Native(NativeFunction::TypedArrayJoin) => {
                    return self.begin_typed_array_join(&site);
                }
                FunctionExecutable::Native(NativeFunction::TypedArraySlice) => {
                    return self.begin_typed_array_slice(&site);
                }
                FunctionExecutable::Native(NativeFunction::TypedArraySubarray) => {
                    return self.begin_typed_array_subarray(&site);
                }
                FunctionExecutable::Native(NativeFunction::TypedArraySearch(direction)) => {
                    return self.begin_typed_array_search(&site, direction);
                }
                FunctionExecutable::Native(NativeFunction::TypedArrayCallback(kind)) => {
                    return self.begin_typed_array_callback(&site, kind);
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::TypedArrayKeys
                    | NativeFunction::TypedArrayValues
                    | NativeFunction::TypedArrayEntries),
                ) => {
                    let kind = match native {
                        NativeFunction::TypedArrayKeys => ArrayIterationKind::Key,
                        NativeFunction::TypedArrayValues => ArrayIterationKind::Value,
                        NativeFunction::TypedArrayEntries => ArrayIterationKind::KeyAndValue,
                        _ => unreachable!("typed array iterator creator match is exhaustive"),
                    };
                    let iterator = self.begin_typed_array_iterator(&site, kind)?;
                    return self.write(site.caller_base, site.destination, iterator);
                }
                FunctionExecutable::Native(NativeFunction::MapConstructor) => {
                    return self.begin_map_from_site(&site);
                }
                FunctionExecutable::Native(NativeFunction::SetConstructor) => {
                    return self.begin_set_from_site(&site);
                }
                FunctionExecutable::Native(NativeFunction::WeakMapConstructor) => {
                    return self.begin_weak_map_from_site(&site);
                }
                FunctionExecutable::Native(NativeFunction::WeakSetConstructor) => {
                    return self.begin_weak_set_from_site(&site);
                }
                FunctionExecutable::Native(NativeFunction::WeakRefConstructor) => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::Native(NativeFunction::WeakRefDeref) => {
                    let target = self.weak_ref_deref(site.this_value)?;
                    return self.write(site.caller_base, site.destination, target);
                }
                FunctionExecutable::Native(NativeFunction::FinalizationRegistryConstructor) => {
                    return Err(ExecutionError::NonConstructor(site.callee));
                }
                FunctionExecutable::Native(NativeFunction::FinalizationRegistryRegister) => {
                    let result = self.finalization_registry_register(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::FinalizationRegistryUnregister) => {
                    let result = self.finalization_registry_unregister(&site)?;
                    return self.write(site.caller_base, site.destination, result);
                }
                FunctionExecutable::Native(NativeFunction::WeakMapGet) => {
                    let value = self.weak_map_get(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::WeakMapSet) => {
                    let value = self.weak_map_set(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::WeakMapGetOrInsert) => {
                    let value = self.weak_map_get_or_insert(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::WeakMapGetOrInsertComputed) => {
                    return self.begin_weak_map_get_or_insert_computed(&site);
                }
                FunctionExecutable::Native(NativeFunction::WeakMapHas) => {
                    let value = self.weak_map_has(&site)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if value {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::WeakMapDelete) => {
                    let value = self.weak_map_delete(&site)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if value {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::WeakSetAdd) => {
                    let value = self.weak_set_add(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::WeakSetHas) => {
                    let value = self.weak_set_has(&site)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if value {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::WeakSetDelete) => {
                    let value = self.weak_set_delete(&site)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if value {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::MapGet) => {
                    let value = self.map_get(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::MapGetOrInsert) => {
                    let value = self.map_get_or_insert(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::MapGetOrInsertComputed) => {
                    return self.begin_map_get_or_insert_computed(&site);
                }
                FunctionExecutable::Native(NativeFunction::MapSet) => {
                    let value = self.map_set(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::MapHas) => {
                    let value = self.map_has(&site)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if value {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::MapDelete) => {
                    let value = self.map_delete(&site)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if value {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::MapClear) => {
                    self.map_clear(site.this_value)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(Immediate::Undefined),
                    );
                }
                FunctionExecutable::Native(NativeFunction::MapSize) => {
                    let value = self.map_size(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::MapKeys) => {
                    let value = self.map_keys(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::MapValues) => {
                    let value = self.map_values(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::MapEntries) => {
                    let value = self.map_entries(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::MapForEach) => {
                    return self.begin_collection_for_each(&site, true);
                }
                FunctionExecutable::Native(NativeFunction::SetAdd) => {
                    let value = self.set_add(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::SetHas) => {
                    let value = self.set_has(&site)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if value {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::SetDelete) => {
                    let value = self.set_delete(&site)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(if value {
                            Immediate::True
                        } else {
                            Immediate::False
                        }),
                    );
                }
                FunctionExecutable::Native(NativeFunction::SetClear) => {
                    self.set_clear(site.this_value)?;
                    return self.write(
                        site.caller_base,
                        site.destination,
                        Value::from_immediate(Immediate::Undefined),
                    );
                }
                FunctionExecutable::Native(NativeFunction::SetSize) => {
                    let value = self.set_size(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::SetValues) => {
                    let value = self.set_values(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::SetEntries) => {
                    let value = self.set_entries(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::SetForEach) => {
                    return self.begin_collection_for_each(&site, false);
                }
                FunctionExecutable::Native(NativeFunction::CollectionIteratorNext) => {
                    let value = self.collection_iterator_next(site.this_value)?;
                    return self.write(site.caller_base, site.destination, value);
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
                FunctionExecutable::Native(NativeFunction::ArrayOf) => {
                    return self.begin_array_of(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayFrom) => {
                    return self.begin_array_from(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayConcat) => {
                    return self.begin_array_concat(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayPush) => {
                    return self.begin_array_insert(&site, false);
                }
                FunctionExecutable::Native(NativeFunction::ArrayJoin) => {
                    return self.begin_array_join(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayToLocaleString) => {
                    return self.begin_array_to_locale_string(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayAt) => {
                    let value = self.array_at(&site)?;
                    return self.write(site.caller_base, site.destination, value);
                }
                FunctionExecutable::Native(NativeFunction::ArrayIndexOf) => {
                    return self.begin_array_index_search(&site, false);
                }
                FunctionExecutable::Native(NativeFunction::ArrayIncludes) => {
                    return self.begin_array_includes(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayPop) => {
                    return self.begin_array_remove(&site, false);
                }
                FunctionExecutable::Native(NativeFunction::ArraySlice) => {
                    return self.begin_array_slice(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayShift) => {
                    return self.begin_array_remove(&site, true);
                }
                FunctionExecutable::Native(NativeFunction::ArrayUnshift) => {
                    return self.begin_array_insert(&site, true);
                }
                FunctionExecutable::Native(NativeFunction::ArrayReverse) => {
                    return self.begin_array_reverse(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayFill) => {
                    return self.begin_array_fill(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayLastIndexOf) => {
                    return self.begin_array_index_search(&site, true);
                }
                FunctionExecutable::Native(NativeFunction::ArrayCopyWithin) => {
                    return self.begin_array_copy_within(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayToReversed) => {
                    return self.begin_array_copy(&site, ArrayCopyKind::ToReversed);
                }
                FunctionExecutable::Native(NativeFunction::ArrayWith) => {
                    return self.begin_array_copy(&site, ArrayCopyKind::With);
                }
                FunctionExecutable::Native(NativeFunction::ArrayToSpliced) => {
                    return self.begin_array_copy(&site, ArrayCopyKind::ToSpliced);
                }
                FunctionExecutable::Native(NativeFunction::ArrayToSorted) => {
                    return self.begin_array_to_sorted(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayFlat) => {
                    return self.begin_array_flat(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayFlatMap) => {
                    return self.begin_array_flat_map(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArraySort) => {
                    return self.begin_array_sort(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayForEach) => {
                    return self.begin_array_for_each(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayEvery) => {
                    return self.begin_array_predicate(&site, true);
                }
                FunctionExecutable::Native(NativeFunction::ArraySome) => {
                    return self.begin_array_predicate(&site, false);
                }
                FunctionExecutable::Native(NativeFunction::ArrayFind) => {
                    return self.begin_array_find(&site, false, false);
                }
                FunctionExecutable::Native(NativeFunction::ArrayFindIndex) => {
                    return self.begin_array_find(&site, false, true);
                }
                FunctionExecutable::Native(NativeFunction::ArrayFindLast) => {
                    return self.begin_array_find(&site, true, false);
                }
                FunctionExecutable::Native(NativeFunction::ArrayFindLastIndex) => {
                    return self.begin_array_find(&site, true, true);
                }
                FunctionExecutable::Native(NativeFunction::ArrayMap) => {
                    return self.begin_array_map(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayFilter) => {
                    return self.begin_array_filter(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayReduce) => {
                    return self.begin_array_reduce(&site, false);
                }
                FunctionExecutable::Native(NativeFunction::ArrayReduceRight) => {
                    return self.begin_array_reduce(&site, true);
                }
                FunctionExecutable::Native(NativeFunction::ArraySplice) => {
                    return self.begin_array_splice(&site);
                }
                FunctionExecutable::Native(NativeFunction::ArrayToString) => {
                    return self.begin_array_join(&site);
                }
                FunctionExecutable::Native(
                    native @ (NativeFunction::ArrayKeys
                    | NativeFunction::ArrayValues
                    | NativeFunction::ArrayEntries),
                ) => {
                    let kind = match native {
                        NativeFunction::ArrayKeys => ArrayIterationKind::Key,
                        NativeFunction::ArrayValues => ArrayIterationKind::Value,
                        NativeFunction::ArrayEntries => ArrayIterationKind::KeyAndValue,
                        _ => unreachable!("array iterator creator match is exhaustive"),
                    };
                    let iterator = self.create_array_iterator(site.this_value, kind)?;
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
                FunctionExecutable::Native(NativeFunction::JsonParse) => {
                    return self.begin_json_parse(&site);
                }
                FunctionExecutable::Native(NativeFunction::JsonStringify) => {
                    return self.begin_json_stringify(&site);
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
                FunctionExecutable::Native(native) if native.global_uri_function().is_some() => {
                    let argument = self.call_argument(&site, 0)?;
                    if argument.is_some_and(|value| self.is_object_value(value)) {
                        return self.dispatch_conversion_native(native, &site, false);
                    }
                    let function = native
                        .global_uri_function()
                        .expect("global-URI guard establishes the native identity");
                    let value = self.global_uri_value(function, &site)?;
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

    /// Reports whether the current staged callable set may be used as a construct newTarget.
    pub(crate) fn is_constructor_value(&mut self, value: Value) -> Result<bool, ExecutionError> {
        if self.is_proxy_value(value) {
            let target = self.proxy_snapshot(value)?.target;
            return self.is_constructor_value(target);
        }
        let function = match self.resolve_function_object(value) {
            Ok(function) => function,
            Err(_) => return Ok(false),
        };
        Ok(match function.executable {
            FunctionExecutable::Native(native) => native.is_constructor(),
            FunctionExecutable::Bound(data) => {
                let target = self.bound_function_snapshot(data)?.bound_target;
                self.is_constructor_value(target)?
            }
            FunctionExecutable::ProxyRevoker(_) => false,
            FunctionExecutable::PromiseResolver { .. } => false,
            FunctionExecutable::PromiseCapabilityExecutor(_) => false,
            FunctionExecutable::PromiseFinallyHandler { .. } => false,
            FunctionExecutable::PromiseFinallyResultHandler { .. } => false,
            FunctionExecutable::PromiseCombinatorHandler { .. } => false,
            FunctionExecutable::AsyncFromSyncIteratorUnwrap { .. } => false,
            FunctionExecutable::AsyncFromSyncIteratorCloseOnReject { .. } => false,
            FunctionExecutable::Bytecode { code, function, .. } => {
                let kind = self
                    .loaded_code(code)?
                    .module
                    .function(function)
                    .ok_or(ExecutionError::MissingEntryFunction(function))?
                    .kind();
                !matches!(
                    kind,
                    FunctionKind::ClassMethod | FunctionKind::ClassFieldInitializer
                )
            }
            FunctionExecutable::ClassBytecode(_) => true,
        })
    }

    /// Reports whether a value has a callable internal method, including nested Proxies.
    pub(crate) fn is_callable_value(&mut self, value: Value) -> Result<bool, ExecutionError> {
        if self.is_proxy_value(value) {
            let target = self.proxy_snapshot(value)?.target;
            return self.is_callable_value(target);
        }
        Ok(self.resolve_function_object(value).is_ok())
    }

    /// Creates the zero-argument dynamic Function through the embedding compiler callback.
    fn create_dynamic_function_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        if site.argument_count != 0 {
            return Err(ExecutionError::UnsupportedDynamicFunctionConstructor);
        }
        let callback = self
            .dynamic_function_callback
            .ok_or(ExecutionError::UnsupportedDynamicFunctionConstructor)?;
        let realm = self.realm_for_callable(site.callee)?;
        callback(self, realm)
    }

    /// Resolves a callable's defining Realm through Proxy and Bound exotic layers.
    #[cold]
    pub(crate) fn realm_for_callable(&mut self, value: Value) -> Result<RealmId, ExecutionError> {
        if self.is_proxy_value(value) {
            let target = self.proxy_snapshot(value)?.target;
            return self.realm_for_callable(target);
        }
        let function = self.resolve_function_object(value)?;
        match function.executable {
            FunctionExecutable::Bytecode { code, .. } => Ok(self.loaded_code(code)?.realm),
            FunctionExecutable::ClassBytecode(data) => {
                let data = self.class_constructor_snapshot(data)?;
                Ok(self.loaded_code(data.code)?.realm)
            }
            FunctionExecutable::Bound(data) => {
                let target = self.bound_function_snapshot(data)?.call_target;
                self.realm_for_callable(target)
            }
            FunctionExecutable::Native(_) => {
                let prototype = function.ordinary.prototype;
                if self.realm.function_prototype == Some(prototype)
                    || self.realm.typed_array_base_constructor == Some(prototype)
                {
                    return Ok(self.active_realm);
                }
                for (id, realm) in &self.inactive_realms {
                    if realm.function_prototype == Some(prototype)
                        || realm.typed_array_base_constructor == Some(prototype)
                    {
                        return Ok(*id);
                    }
                }
                Ok(self.active_realm)
            }
            _ => Ok(self.active_realm),
        }
    }

    /// Copies only callable dispatch metadata through a checked no-GC borrow on the hot path.
    #[inline(always)]
    pub(crate) fn resolve_function_executable(
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
        if let Some(source) = site.argument_source {
            return self.native_call_state_snapshot(source).and_then(|state| {
                state
                    .argument(suffix_index)
                    .map(Some)
                    .ok_or(ExecutionError::InvalidRegister(RegisterId::new(
                        suffix_index,
                    )))
            });
        }
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

    /// Materializes the active frame's trailing positional arguments as one packed ordinary Array.
    fn collect_rest_arguments(&mut self, start: u32) -> Result<Value, ExecutionError> {
        let frame = *self
            .fiber
            .frames
            .last()
            .ok_or(ExecutionError::MissingEnvironment)?;
        let prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before rest parameter collection");
        let result = self.create_array_object_with_prototype(prototype)?;
        let site = CallSite {
            caller_base: frame.base,
            destination: 0,
            callee: Value::from_immediate(Immediate::Undefined),
            argument_base: frame.argument_base,
            argument_source: self.fiber.argument_sources.last().copied().flatten(),
            argument_prefix: frame.argument_prefix,
            argument_prefix_offset: frame.argument_prefix_offset,
            argument_prefix_count: frame.argument_prefix_count,
            argument_count: frame.argument_count,
            this_value: frame.this_value,
            new_target: frame.new_target,
            construct_receiver: (frame.new_target.as_immediate() != Some(Immediate::Undefined))
                .then_some(frame.receiver_or_home_object)
                .flatten(),
            call_site: frame.call_site.unwrap_or(WordOffset::new(0)),
        };
        let mut output_index = 0_u32;
        for input_index in start..site.argument_count {
            let value = self
                .call_argument(&site, input_index)?
                .expect("rest argument index stays below the exact argument count");
            let key = self.safe_integer_property_atom(u64::from(output_index))?;
            self.set_own_data_property(result, key, value)?;
            output_index = output_index
                .checked_add(1)
                .ok_or(ExecutionError::ArrayLengthOverflow)?;
        }
        let length = self.intern_intrinsic_name(b"length")?;
        self.set_own_data_property(result, length, safe_integer_value(u64::from(output_index)))?;
        Ok(result)
    }

    /// Lazily creates one identity-stable unmapped arguments object for the active activation.
    fn materialize_arguments_object(&mut self) -> Result<Value, ExecutionError> {
        if let Some(arguments) = self.fiber.argument_objects.last().copied().flatten() {
            return Ok(arguments);
        }
        let frame = *self
            .fiber
            .frames
            .last()
            .ok_or(ExecutionError::MissingEnvironment)?;
        let mapped = if frame.strictness == FunctionStrictness::Sloppy {
            let function = self
                .loaded_code(frame.code)?
                .module
                .function(frame.function)
                .ok_or(ExecutionError::MissingEntryFunction(frame.function))?;
            let layout = function.layout();
            (layout.function_length == layout.argument_count
                && !layout.has_rest_parameter
                && layout.simple_parameter_list)
                .then_some((
                    u32::try_from(self.fiber.frames.len() - 1)
                        .map_err(|_| ExecutionError::RegisterAllocationFailed)?,
                    frame.base,
                    layout.argument_count,
                    frame.code,
                    frame.function,
                ))
        } else {
            None
        };
        let mapped_values = if mapped.is_some() {
            let count = mapped.map_or(0, |mapping| mapping.2);
            let mut values = Vec::new();
            values
                .try_reserve_exact(count as usize)
                .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
            for index in 0..count {
                values.push(self.read(frame.base, index)?);
            }
            Some(values)
        } else {
            None
        };
        let arguments =
            self.allocate_arguments_object(mapped, frame.strictness == FunctionStrictness::Strict)?;
        if let Some(values) = mapped_values.as_ref() {
            for (index, value) in values.iter().copied().enumerate() {
                self.write(
                    frame.base,
                    u32::try_from(index).map_err(|_| ExecutionError::RegisterAllocationFailed)?,
                    value,
                )?;
            }
        }
        let site = CallSite {
            caller_base: frame.base,
            destination: 0,
            callee: Value::from_immediate(Immediate::Undefined),
            argument_base: frame.argument_base,
            argument_source: self.fiber.argument_sources.last().copied().flatten(),
            argument_prefix: frame.argument_prefix,
            argument_prefix_offset: frame.argument_prefix_offset,
            argument_prefix_count: frame.argument_prefix_count,
            argument_count: frame.argument_count,
            this_value: frame.this_value,
            new_target: frame.new_target,
            construct_receiver: (frame.new_target.as_immediate() != Some(Immediate::Undefined))
                .then_some(frame.receiver_or_home_object)
                .flatten(),
            call_site: frame.call_site.unwrap_or(WordOffset::new(0)),
        };
        for index in 0..site.argument_count {
            let value = match mapped_values
                .as_ref()
                .and_then(|values| values.get(index as usize).copied())
            {
                Some(value) => value,
                None => self
                    .call_argument(&site, index)?
                    .expect("arguments index stays below the exact argument count"),
            };
            let key = self.safe_integer_property_atom(u64::from(index))?;
            self.set_own_data_property(arguments, key, value)?;
        }
        let length = self.length_atom()?;
        self.define_data_property(
            arguments,
            length,
            DataPropertyDescriptor {
                value: Some(safe_integer_value(u64::from(site.argument_count))),
                writable: Some(true),
                enumerable: Some(false),
                configurable: Some(true),
            },
        )?;
        if frame.strictness == FunctionStrictness::Sloppy
            && let Some(callee) = self.fiber.argument_callees.last().copied().flatten()
        {
            let callee_atom = self.intern_intrinsic_name(b"callee")?;
            self.define_data_property(
                arguments,
                callee_atom,
                DataPropertyDescriptor {
                    value: Some(callee),
                    writable: Some(true),
                    enumerable: Some(false),
                    configurable: Some(true),
                },
            )?;
        }
        if let (Some(iterator), Some(values)) = (
            self.realm.well_known_symbols.iterator,
            self.realm.array_values,
        ) {
            let key = self.property_key(iterator)?;
            self.define_data_property(
                arguments,
                key,
                DataPropertyDescriptor {
                    value: Some(values),
                    writable: Some(true),
                    enumerable: Some(false),
                    configurable: Some(true),
                },
            )?;
        }
        *self
            .fiber
            .argument_objects
            .last_mut()
            .expect("arguments cache stays aligned with active frames") = Some(arguments);
        Ok(arguments)
    }

    /// Reserves the callee state before mutation, then copies the supplied positional arguments.
    pub(crate) fn push_call_frame(
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
        if target.kind == FunctionKind::DerivedClassConstructor {
            self.fiber
                .derived_activations
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::FrameAllocationFailed)?;
        }
        if target.kind == FunctionKind::BaseClassConstructor {
            self.fiber
                .base_class_activations
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
        let receiver_or_home_object = if matches!(
            target.kind,
            FunctionKind::ClassMethod | FunctionKind::ClassFieldInitializer
        ) {
            Some(self.function_home_object(site.callee)?)
        } else {
            site.construct_receiver
        };
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
            receiver_or_home_object,
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
        self.fiber.argument_objects.push(None);
        self.fiber.argument_sources.push(site.argument_source);
        self.fiber.argument_callees.push(Some(site.callee));
        if target.kind == FunctionKind::DerivedClassConstructor {
            self.fiber.derived_activations.push(ClassActivation {
                frame_depth: self.fiber.frames.len() as u32,
                function: site.callee,
            });
            self.fiber
                .frames
                .last_mut()
                .expect("derived activation retains its frame")
                .this_value = Value::from_immediate(Immediate::Uninitialized);
        }
        if target.kind == FunctionKind::BaseClassConstructor {
            self.fiber.base_class_activations.push(ClassActivation {
                frame_depth: self.fiber.frames.len() as u32,
                function: site.callee,
            });
        }
        if let Some(slot_count) = NonZeroU32::new(target.layout.environment_slot_count)
            && let Err(error) = self.allocate_current_environment(
                target.kind,
                slot_count,
                target
                    .layout
                    .self_binding_slot
                    .map(|slot| (slot, site.callee)),
            )
        {
            self.fiber.frames.pop();
            self.discard_exited_class_environments();
            self.fiber.argument_objects.pop();
            self.fiber.argument_sources.pop();
            self.fiber.argument_callees.pop();
            if self
                .fiber
                .derived_activations
                .last()
                .is_some_and(|activation| activation.frame_depth as usize > self.fiber.frames.len())
            {
                self.fiber.derived_activations.pop();
            }
            if self
                .fiber
                .base_class_activations
                .last()
                .is_some_and(|activation| activation.frame_depth as usize > self.fiber.frames.len())
            {
                self.fiber.base_class_activations.pop();
            }
            self.fiber.registers.truncate(callee_base as usize);
            return Err(error);
        }
        Ok(())
    }

    #[inline(always)]
    pub(crate) fn bind_ordinary_this(
        &self,
        strictness: FunctionStrictness,
        this_argument: Value,
    ) -> Value {
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
        if self.fiber.frames.len() == 1
            && !self
                .fiber
                .frames
                .last()
                .is_some_and(|frame| frame.return_continuation)
        {
            return Ok(Some(RunOutcome::Completed(value)));
        }
        self.return_from_callee(value)
    }

    /// Pops a non-entry frame and restores caller checkpoints on the ordinary call hot path.
    #[inline(always)]
    fn return_from_callee(&mut self, value: Value) -> Result<Option<RunOutcome>, ExecutionError> {
        let derived = self
            .fiber
            .derived_activations
            .last()
            .copied()
            .is_some_and(|activation| activation.frame_depth as usize == self.fiber.frames.len());
        let value = if derived {
            if self.is_object_value(value) {
                value
            } else if value.as_immediate() == Some(Immediate::Undefined) {
                let this_value = self
                    .fiber
                    .frames
                    .last()
                    .expect("derived return retains its frame")
                    .this_value;
                if this_value.as_immediate() == Some(Immediate::Uninitialized) {
                    return Err(ExecutionError::UninitializedThis);
                }
                this_value
            } else {
                return Err(ExecutionError::InvalidDerivedConstructorReturn(value));
            }
        } else {
            value
        };
        if let Some(arguments) = self.fiber.argument_objects.last().copied().flatten() {
            self.detach_mapped_arguments(arguments)?;
        }
        let frame = self
            .fiber
            .frames
            .pop()
            .expect("callee return always has an active frame");
        self.discard_exited_class_environments();
        self.fiber
            .argument_objects
            .pop()
            .expect("arguments cache stays aligned with active frames");
        self.fiber
            .argument_sources
            .pop()
            .expect("argument sources stay aligned with active frames");
        self.fiber
            .argument_callees
            .pop()
            .expect("argument callees stay aligned with active frames");
        if derived {
            self.fiber.derived_activations.pop();
        }
        if self
            .fiber
            .base_class_activations
            .last()
            .is_some_and(|activation| activation.frame_depth as usize > self.fiber.frames.len())
        {
            self.fiber.base_class_activations.pop();
        }
        let value = match frame.receiver_or_home_object {
            Some(receiver)
                if frame.new_target.as_immediate() != Some(Immediate::Undefined)
                    && !self.is_object_value(value) =>
            {
                receiver
            }
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
            if let NativeContinuationKind::CollectionIteratorClose(stage) = continuation.kind() {
                return self.resume_collection_iterator_close(continuation, stage, value);
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
                    } else if mode == PropertyCallbackMode::CopyDataProperties {
                        let state =
                            self.pending_copy_data_properties_reference(continuation.first())?;
                        self.resume_copy_data_properties(site, state, value)
                            .map(|_| ())
                    } else if mode == PropertyCallbackMode::DefineProperties {
                        let state =
                            self.pending_define_properties_reference(continuation.first())?;
                        self.resume_define_properties_descriptor_get(site, state, value)
                    } else if mode == PropertyCallbackMode::ArgumentList {
                        let state = self.pending_argument_list_reference(continuation.first())?;
                        self.resume_argument_list(site, state, value)
                    } else {
                        self.write(site.caller_base, site.destination, value)
                    }
                }
                NativeContinuationKind::PropertySet(PropertyWriteMode::Assignment) => {
                    let receiver = continuation.first();
                    let assigned = continuation.second();
                    self.write(site.caller_base, site.destination, assigned)?;
                    self.finish_property_write(receiver, true)
                }
                NativeContinuationKind::PropertySet(PropertyWriteMode::Reflect) => {
                    self.write(site.caller_base, site.destination, boolean_value(true))
                }
                NativeContinuationKind::PropertySet(PropertyWriteMode::ObjectAssign) => {
                    let state =
                        self.pending_copy_data_properties_reference(continuation.first())?;
                    self.resume_object_assign_set(site, state).map(|_| ())
                }
                NativeContinuationKind::Proxy { operation, stage } => self
                    .resume_proxy_internal_method(continuation, operation, stage, value)
                    .map(|_| ()),
                NativeContinuationKind::ProxyCall { stage, .. } => self
                    .resume_proxy_call(continuation, stage, value)
                    .map(|_| ()),
                NativeContinuationKind::ProxySetPrototype { mode, stage } => self
                    .resume_proxy_set_prototype(continuation, mode, stage, value)
                    .map(|_| ()),
                NativeContinuationKind::ProxySet { mode, stage } => self
                    .resume_proxy_set(continuation, mode, stage, value)
                    .map(|_| ()),
                NativeContinuationKind::ProxyHas(stage) => self
                    .resume_proxy_has(continuation, stage, value)
                    .map(|_| ()),
                NativeContinuationKind::ProxyGetOwn { mode, stage } => self
                    .resume_proxy_get_own(continuation, mode, stage, value)
                    .map(|_| ()),
                NativeContinuationKind::ProxyGet(stage) => self
                    .resume_proxy_get(continuation, stage, value)
                    .map(|_| ()),
                NativeContinuationKind::ProxyDelete { mode, stage } => self
                    .resume_proxy_delete(continuation, mode, stage, value)
                    .map(|_| ()),
                NativeContinuationKind::ProxyDefine { mode, stage } => self
                    .resume_proxy_define(continuation, mode, stage, value)
                    .map(|_| ()),
                NativeContinuationKind::ProxyOwnKeys { mode, stage } => self
                    .resume_proxy_own_keys(continuation, mode, stage, value)
                    .map(|_| ()),
                NativeContinuationKind::CollectionInitializer(stage) => {
                    let state =
                        self.pending_collection_initializer_reference(continuation.first())?;
                    self.resume_collection_initializer(site, state, stage, value)
                }
                NativeContinuationKind::CollectionIteratorClose(_) => {
                    unreachable!("iterator close resumes before native dispatch")
                }
                NativeContinuationKind::CopyDataProperties(stage) => self
                    .resume_copy_data_properties_stage(continuation, stage, value)
                    .map(|_| ()),
                NativeContinuationKind::DefineProperties(stage) => self
                    .resume_define_properties_stage(continuation, stage, value)
                    .map(|_| ()),
                NativeContinuationKind::GetOwnPropertyDescriptors(stage) => self
                    .resume_get_own_property_descriptors(continuation, stage, value)
                    .map(|_| ()),
                NativeContinuationKind::CollectionForEach => {
                    let state = self.pending_collection_for_each_reference(continuation.first())?;
                    self.resume_collection_for_each(site, state)
                }
                NativeContinuationKind::ArrayForEach(stage) => {
                    let state = self.native_call_state_reference(continuation.first())?;
                    self.resume_array_for_each(site, state, stage, value, continuation.second())
                }
                NativeContinuationKind::TypedArrayCallback(stage) => {
                    let state = self.native_call_state_reference(continuation.first())?;
                    self.resume_typed_array_callback(
                        site,
                        state,
                        stage,
                        value,
                        continuation.second(),
                    )
                }
                NativeContinuationKind::ArrayConcat(stage) => {
                    let state = self.pending_array_concat_reference(continuation.first())?;
                    self.resume_array_concat(site, state, stage, value)
                }
                NativeContinuationKind::ArrayFlat(stage) => {
                    let state = self.pending_array_flat_reference(continuation.first())?;
                    self.resume_array_flat(site, state, stage, value)
                }
                NativeContinuationKind::ArrayFlatMap(stage) => {
                    let state = self.pending_array_flat_map_reference(continuation.first())?;
                    self.resume_array_flat_map(site, state, stage, value)
                }
                NativeContinuationKind::ArrayCopy(stage) => {
                    let state = self.pending_array_copy_reference(continuation.first())?;
                    self.resume_array_copy(site, state, stage, value)
                }
                NativeContinuationKind::ArrayCopyWithin(stage) => {
                    let state = self.pending_array_copy_within_reference(continuation.first())?;
                    self.resume_array_copy_within(site, state, stage, value)
                }
                NativeContinuationKind::ArrayToSorted(stage) => {
                    let state = self.pending_array_to_sorted_reference(continuation.first())?;
                    self.resume_array_to_sorted(site, state, stage, value)
                }
                NativeContinuationKind::ArraySlice(stage) => {
                    let state = self.pending_array_slice_reference(continuation.first())?;
                    self.resume_array_slice(site, state, stage, value)
                }
                NativeContinuationKind::ArrayBufferSlice(stage) => {
                    let state = self.native_call_state_reference(continuation.first())?;
                    self.resume_array_buffer_slice(site, state, stage, value)
                }
                NativeContinuationKind::ArraySplice(stage) => {
                    let state = self.pending_array_splice_reference(continuation.first())?;
                    self.resume_array_splice(site, state, stage, value, continuation.second())
                }
                NativeContinuationKind::ArrayRemove(stage) => {
                    let state = self.pending_array_remove_reference(continuation.first())?;
                    self.resume_array_remove(site, state, stage, value)
                }
                NativeContinuationKind::ArrayInsert(stage) => {
                    let state = self.pending_array_insert_reference(continuation.first())?;
                    self.resume_array_insert(site, state, stage, value)
                }
                NativeContinuationKind::ArrayReverse(stage) => {
                    let state = self.pending_array_reverse_reference(continuation.first())?;
                    self.resume_array_reverse(site, state, stage, value)
                }
                NativeContinuationKind::ArrayFill(stage) => {
                    let state = self.pending_array_fill_reference(continuation.first())?;
                    self.resume_array_fill(site, state, stage, value)
                }
                NativeContinuationKind::ArrayJoin(stage) => {
                    let state = self.pending_array_join_reference(continuation.first())?;
                    self.resume_array_join(site, state, stage, value)
                }
                NativeContinuationKind::ArrayStatic(stage) => {
                    let state = self.pending_array_static_reference(continuation.first())?;
                    self.resume_array_static(site, state, stage, value)
                }
                NativeContinuationKind::MapGetOrInsertComputed => {
                    let state = self.pending_map_upsert_reference(continuation.first())?;
                    self.resume_map_get_or_insert_computed(site, state, value)
                }
                NativeContinuationKind::InstanceElements(stage) => {
                    self.resume_instance_elements(continuation, stage, value)
                }
                NativeContinuationKind::InstanceOf => {
                    self.resume_instance_of(continuation, value).map(|_| ())
                }
                NativeContinuationKind::ErrorConstructor(stage) => {
                    self.resume_error_constructor(continuation, stage, value)
                }
                NativeContinuationKind::ErrorToString(stage) => {
                    self.resume_error_to_string(continuation, stage, value)
                }
                NativeContinuationKind::ErrorStackSetter(stage) => {
                    self.resume_error_stack_setter(continuation, stage, value)
                }
                NativeContinuationKind::ObjectToString => {
                    self.resume_object_to_string(continuation, value)
                }
                NativeContinuationKind::ObjectIsPrototypeOf => self
                    .resume_object_is_prototype_of(continuation, value)
                    .map(|_| ()),
                NativeContinuationKind::ObjectLookupAccessor { .. } => self
                    .resume_object_lookup_accessor(continuation, value)
                    .map(|_| ()),
                NativeContinuationKind::ObjectToLocaleString(stage) => {
                    self.resume_object_to_locale_string(continuation, stage, value)
                }
                NativeContinuationKind::DateToJson(stage) => {
                    self.resume_date_to_json(continuation, stage, value)
                }
                NativeContinuationKind::RegExpTest(stage) => {
                    self.resume_regexp_test(continuation, stage, value)
                }
                NativeContinuationKind::RegExpSearch(stage) => {
                    self.resume_regexp_search(continuation, stage, value)
                }
                NativeContinuationKind::RegExpStringIterator(stage) => {
                    self.resume_regexp_string_iterator(continuation, stage, value)
                }
                NativeContinuationKind::RegExpReplace => {
                    self.resume_regexp_replace_callback(continuation, value)
                }
                NativeContinuationKind::RegExpFlags(index) => {
                    self.resume_regexp_flags(continuation, index, value)
                }
                NativeContinuationKind::StringSplit(stage) => {
                    self.resume_string_split(continuation, stage, value)
                }
                NativeContinuationKind::StringReplaceAll(stage) => {
                    self.resume_string_replace_all(continuation, stage, value)
                }
                NativeContinuationKind::TypedArrayConstruction(stage) => {
                    self.resume_typed_array_construction(continuation, stage, value)
                }
                NativeContinuationKind::TypedArraySet(stage) => {
                    self.resume_typed_array_set(continuation, stage, value)
                }
                NativeContinuationKind::TypedArraySlice(stage) => {
                    let state = self.native_call_state_reference(continuation.first())?;
                    self.resume_typed_array_slice(continuation.site(), state, stage, value)
                }
                NativeContinuationKind::TypedArraySubarray(stage) => {
                    let state = self.native_call_state_reference(continuation.first())?;
                    self.resume_typed_array_subarray(continuation.site(), state, stage, value)
                }
                NativeContinuationKind::JsonStringify(stage) => {
                    self.resume_json_stringify(continuation, stage, value)
                }
                NativeContinuationKind::JsonParseReviver => {
                    self.write(site.caller_base, site.destination, value)
                }
                NativeContinuationKind::SignalState(stage) => {
                    self.resume_signal_state(continuation, stage, value)
                }
                NativeContinuationKind::SignalWatcherHook => {
                    self.resume_signal_watcher_hook(continuation)
                }
                NativeContinuationKind::SignalComputed => {
                    self.resume_signal_computed(continuation, value)
                }
                NativeContinuationKind::SignalUntrack => {
                    self.resume_signal_untrack(continuation, value)
                }
                NativeContinuationKind::GeneratorResume => {
                    self.finish_generator_return(continuation, value)
                }
                NativeContinuationKind::AsyncFunction => {
                    self.finish_async_function_return(continuation, value)
                }
                NativeContinuationKind::AsyncAwaitConstructor => {
                    self.resume_async_await_constructor(continuation, value)
                }
                NativeContinuationKind::AsyncFromSyncIterator(stage) => {
                    self.resume_async_from_sync_iterator(continuation, stage, value)
                }
                NativeContinuationKind::AsyncFromSyncCloseOnReject(stage) => {
                    self.resume_async_from_sync_close_on_reject(continuation, stage, value)
                }
                NativeContinuationKind::PromiseExecutor => {
                    self.write(site.caller_base, site.destination, continuation.first())
                }
                NativeContinuationKind::PromiseReaction => {
                    self.finish_promise_reaction(continuation, value)
                }
                NativeContinuationKind::PromiseCapabilityCall => {
                    self.finish_promise_capability_call(continuation)
                }
                NativeContinuationKind::PromiseThen(stage) => {
                    let state = self.native_call_state_reference(continuation.first())?;
                    self.resume_promise_then(site, state, stage, value)
                }
                NativeContinuationKind::PromiseFinally => {
                    self.finish_promise_finally_callback(continuation, value)
                }
                NativeContinuationKind::PromiseFinallyMethod(stage) => {
                    let state = self.native_call_state_reference(continuation.first())?;
                    self.resume_promise_finally_method(site, state, stage, value)
                }
                NativeContinuationKind::PromiseCatch(stage) => {
                    let state = self.native_call_state_reference(continuation.first())?;
                    self.resume_promise_catch(site, state, stage, value)
                }
                NativeContinuationKind::PromiseFinallyResolve => {
                    let state = self.native_call_state_reference(continuation.first())?;
                    self.finish_promise_finally_resolved(continuation, state, value)
                }
                NativeContinuationKind::PromiseStaticResolve(
                    PromiseStaticResolveStage::ConstructorPrototype,
                ) => self.resume_promise_constructor(continuation, value),
                NativeContinuationKind::PromiseStaticResolve(stage) => {
                    self.resume_generic_promise_resolve(continuation, stage, value)
                }
                NativeContinuationKind::PromiseCombinator(stage) => {
                    let state = self.pending_promise_combinator_reference(continuation.first())?;
                    self.resume_promise_combinator(site, state, stage, value)
                }
                NativeContinuationKind::PromiseResolution(mode) => {
                    self.finish_promise_resolution(continuation, mode, value)
                }
                NativeContinuationKind::PromiseThenable => {
                    self.finish_promise_thenable(continuation)
                }
                NativeContinuationKind::FinalizationCleanup => {
                    self.finish_finalization_cleanup_job();
                    self.fiber
                        .frames
                        .last_mut()
                        .ok_or(ExecutionError::MissingEnvironment)?
                        .pc = site.call_site;
                    Ok(())
                }
                NativeContinuationKind::ConversionCallRoot => {
                    unreachable!("conversion call roots resume before native dispatch")
                }
            };
            if let Err(error) = result {
                if matches!(
                    continuation.kind(),
                    NativeContinuationKind::AsyncFromSyncIterator(_)
                ) {
                    let state = self.native_call_state_reference(continuation.first())?;
                    let reason = match error {
                        ExecutionError::HostThrown(value) => value,
                        error => {
                            let Some(kind) = execution_error_kind(&error) else {
                                return Err(error);
                            };
                            self.create_native_error(kind, None)?
                        }
                    };
                    self.reject_async_from_sync(site, state, reason)?;
                    return Ok(None);
                }
                if matches!(
                    continuation.kind(),
                    NativeContinuationKind::AsyncFromSyncCloseOnReject(_)
                ) {
                    self.finish_async_from_sync_close_on_reject(continuation.second())?;
                    return Ok(None);
                }
                if continuation.kind() == NativeContinuationKind::SignalWatcherHook
                    && let ExecutionError::HostThrown(thrown) = &error
                {
                    return self.throw_value(*thrown, site.call_site);
                }
                if matches!(
                    continuation.kind(),
                    NativeContinuationKind::PromiseCombinator(stage)
                        if stage != PromiseCombinatorStage::CapabilityConstructor
                ) && self.reject_promise_combinator_execution_error(continuation, &error)?
                {
                    return Ok(None);
                }
                if let Some(iterator) = self.array_static_close_iterator(continuation)?
                    && let Some(kind) = execution_error_kind(&error)
                {
                    let original_throw = self.create_native_error(kind, None)?;
                    return self.begin_iterator_close(site, iterator, original_throw);
                }
                if let Some(state) = self.collection_initializer_close_state(continuation)?
                    && let Some(kind) = execution_error_kind(&error)
                {
                    let original_throw = self.create_native_error(kind, None)?;
                    return self.begin_collection_iterator_close(site, state, original_throw);
                }
                let Some(kind) = execution_error_kind(&error) else {
                    return Err(error);
                };
                return self.throw_native_error(kind, site.call_site);
            }
            let restored_sync_generator_caller = continuation.kind()
                == NativeContinuationKind::GeneratorResume
                && continuation.second().as_immediate() == Some(Immediate::Undefined);
            if self.fiber.frames.len() != frame_depth && !restored_sync_generator_caller {
                return Ok(None);
            }
            if continuation.kind()
                == NativeContinuationKind::RegExpSearch(RegExpSearchStage::ExecCall)
                && self.fiber.completions.last_native().is_some_and(|parent| {
                    matches!(
                        parent.kind(),
                        NativeContinuationKind::RegExpSearch(
                            RegExpSearchStage::StringMethodCall
                                | RegExpSearchStage::StringCreatedMethodCall
                        )
                    )
                })
            {
                // The parent belongs to the enclosing @@search call frame, not this exec return.
                return Ok(None);
            }
            let frame_completion_base = self
                .fiber
                .frames
                .last()
                .ok_or(ExecutionError::MissingEnvironment)?
                .completion_base as usize;
            if self.fiber.completions.len() <= frame_completion_base {
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

    /// Drains a native caller immediately after a Yield opcode restores its Fiber.
    fn resume_restored_generator_native_caller(
        &mut self,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let frame_completion_base = self
            .fiber
            .frames
            .last()
            .ok_or(ExecutionError::MissingEnvironment)?
            .completion_base as usize;
        if self.fiber.completions.len() <= frame_completion_base {
            return Ok(None);
        }
        let Some(continuation) = self.fiber.completions.pop_native() else {
            return Ok(None);
        };
        let site = continuation.site();
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_native_continuation(continuation, value)
    }

    /// Propagates a thrown value through explicit frames until an immutable handler range matches.
    #[cold]
    #[inline(never)]
    pub(crate) fn throw_value(
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
    pub(crate) fn dispatch_abrupt(
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
                self.restore_class_environment_depth(handler.environment_depth)?;
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
                    if self.fiber.frames.len() == 1 && !frame.return_continuation {
                        return self.promise_checkpoint(value, instruction_offset);
                    }
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
            if self.fiber.frames.len() == 1 && !frame.return_continuation {
                return Ok(self.unhandled_throw(value));
            }
            if let Some(arguments) = self.fiber.argument_objects.last().copied().flatten() {
                self.detach_mapped_arguments(arguments)?;
            }
            let frame = self
                .fiber
                .frames
                .pop()
                .expect("non-entry abrupt completion retains a callee frame");
            self.discard_exited_class_environments();
            self.fiber
                .argument_objects
                .pop()
                .expect("arguments cache stays aligned with abrupt frame unwinding");
            self.fiber
                .argument_sources
                .pop()
                .expect("argument sources stay aligned with abrupt frame unwinding");
            self.fiber
                .argument_callees
                .pop()
                .expect("argument callees stay aligned with abrupt frame unwinding");
            if self
                .fiber
                .derived_activations
                .last()
                .is_some_and(|activation| activation.frame_depth as usize > self.fiber.frames.len())
            {
                self.fiber.derived_activations.pop();
            }
            if self
                .fiber
                .base_class_activations
                .last()
                .is_some_and(|activation| activation.frame_depth as usize > self.fiber.frames.len())
            {
                self.fiber.base_class_activations.pop();
            }
            self.fiber.registers.truncate(frame.base as usize);
            self.fiber.handlers.truncate(frame.handler_base as usize);
            self.fiber
                .completions
                .truncate(frame.completion_base as usize);
            if frame.return_continuation {
                let continuation = self.pop_native_continuation()?;
                if continuation.kind()
                    == NativeContinuationKind::CollectionIteratorClose(
                        CollectionIteratorCloseStage::ReturnCall,
                    )
                {
                    completion = CompletionRecord::throw(continuation.second());
                    instruction_offset = continuation.site().call_site;
                    continue;
                }
                if let Some(state) = self.collection_initializer_close_state(continuation)? {
                    return self.begin_collection_iterator_close(continuation.site(), state, value);
                }
                if let Some(iterator) = self.array_static_close_iterator(continuation)? {
                    return self.begin_iterator_close(continuation.site(), iterator, value);
                }
                if continuation.kind() == NativeContinuationKind::SignalWatcherHook {
                    let site = continuation.site();
                    match self.continue_signal_watcher_hook_abrupt(continuation, value)? {
                        None => return Ok(None),
                        Some(replacement) => {
                            completion = CompletionRecord::throw(replacement);
                            instruction_offset = site.call_site;
                            continue;
                        }
                    }
                }
                if continuation.kind() == NativeContinuationKind::SignalComputed {
                    let site = continuation.site();
                    match self.continue_signal_computed_abrupt(continuation, value)? {
                        None => return Ok(None),
                        Some(replacement) => {
                            completion = CompletionRecord::throw(replacement);
                            instruction_offset = site.call_site;
                            continue;
                        }
                    }
                }
                if continuation.kind() == NativeContinuationKind::SignalUntrack {
                    let site = continuation.site();
                    self.continue_signal_untrack_abrupt(continuation);
                    instruction_offset = site.call_site;
                    continue;
                }
                if continuation.kind() == NativeContinuationKind::GeneratorResume {
                    if self.finish_generator_throw(continuation, value)? {
                        return Ok(None);
                    }
                    instruction_offset = continuation.site().call_site;
                    continue;
                }
                if continuation.kind() == NativeContinuationKind::AsyncFunction {
                    self.finish_async_function_throw(continuation, value)?;
                    return Ok(None);
                }
                if continuation.kind() == NativeContinuationKind::AsyncAwaitConstructor {
                    self.reject_async_await_constructor(continuation, value)?;
                    return Ok(None);
                }
                if matches!(
                    continuation.kind(),
                    NativeContinuationKind::AsyncFromSyncIterator(_)
                ) {
                    let state = self.native_call_state_reference(continuation.first())?;
                    self.reject_async_from_sync(continuation.site(), state, value)?;
                    return Ok(None);
                }
                if matches!(
                    continuation.kind(),
                    NativeContinuationKind::AsyncFromSyncCloseOnReject(_)
                ) {
                    self.finish_async_from_sync_close_on_reject(continuation.second())?;
                    return Ok(None);
                }
                if continuation.kind() == NativeContinuationKind::PromiseExecutor {
                    self.reject_promise_executor(continuation, value)?;
                    let site = continuation.site();
                    self.write(site.caller_base, site.destination, continuation.first())?;
                    return Ok(None);
                }
                if continuation.kind() == NativeContinuationKind::PromiseReaction {
                    self.begin_promise_reaction_rejection(continuation, value)?;
                    return Ok(None);
                }
                if continuation.kind() == NativeContinuationKind::PromiseFinally {
                    if let Some(parent) = self.fiber.completions.last_native()
                        && parent.kind() == NativeContinuationKind::PromiseReaction
                    {
                        let parent = self.pop_native_continuation()?;
                        self.begin_promise_reaction_rejection(parent, value)?;
                        return Ok(None);
                    }
                    return Err(ExecutionError::MissingNativeContinuation);
                }
                if continuation.kind() == NativeContinuationKind::PromiseCapabilityCall {
                    self.promise_jobs.finish_active();
                    return Ok(self.unhandled_throw(value));
                }
                if let NativeContinuationKind::PromiseResolution(mode) = continuation.kind() {
                    self.reject_promise_resolution(continuation, mode, value)?;
                    return Ok(None);
                }
                if continuation.kind() == NativeContinuationKind::PromiseThenable {
                    self.reject_promise_thenable(continuation, value)?;
                    return Ok(None);
                }
                if continuation.kind()
                    == NativeContinuationKind::PromiseStaticResolve(
                        PromiseStaticResolveStage::TryCallback,
                    )
                {
                    self.reject_promise_try_callback(continuation, value)?;
                    return Ok(None);
                }
                if continuation.kind() == NativeContinuationKind::FinalizationCleanup {
                    self.finish_finalization_cleanup_job();
                }
                if matches!(
                    continuation.kind(),
                    NativeContinuationKind::PromiseCombinator(stage)
                        if stage != PromiseCombinatorStage::CapabilityConstructor
                ) {
                    self.reject_or_close_promise_combinator(continuation, value)?;
                    return Ok(None);
                }
                if let Some(parent) = self.fiber.completions.last_native()
                    && let NativeContinuationKind::PromiseResolution(mode) = parent.kind()
                {
                    let parent = self.pop_native_continuation()?;
                    self.reject_promise_resolution(parent, mode, value)?;
                    return Ok(None);
                }
                if let Some(parent) = self.fiber.completions.last_native()
                    && matches!(
                        parent.kind(),
                        NativeContinuationKind::PromiseCombinator(stage)
                            if stage != PromiseCombinatorStage::CapabilityConstructor
                    )
                {
                    let parent = self.pop_native_continuation()?;
                    self.reject_or_close_promise_combinator(parent, value)?;
                    return Ok(None);
                }
                if let Some(parent) = self.fiber.completions.last_native()
                    && parent.kind() == NativeContinuationKind::AsyncAwaitConstructor
                {
                    let parent = self.pop_native_continuation()?;
                    self.reject_async_await_constructor(parent, value)?;
                    return Ok(None);
                }
                if let Some(parent) = self.fiber.completions.last_native()
                    && matches!(
                        parent.kind(),
                        NativeContinuationKind::AsyncFromSyncIterator(_)
                    )
                {
                    let parent = self.pop_native_continuation()?;
                    let state = self.native_call_state_reference(parent.first())?;
                    self.reject_async_from_sync(parent.site(), state, value)?;
                    return Ok(None);
                }
                if let Some(parent) = self.fiber.completions.last_native()
                    && matches!(
                        parent.kind(),
                        NativeContinuationKind::AsyncFromSyncCloseOnReject(_)
                    )
                {
                    let parent = self.pop_native_continuation()?;
                    self.finish_async_from_sync_close_on_reject(parent.second())?;
                    return Ok(None);
                }
            }
            instruction_offset = frame
                .call_site
                .expect("every non-entry frame records its caller call-site");
        }
    }

    /// Recovers iterable-consumer state from direct or ToPropertyKey continuations.
    fn collection_initializer_close_state(
        &mut self,
        continuation: NativeContinuation,
    ) -> Result<Option<Value>, ExecutionError> {
        if let NativeContinuationKind::CollectionInitializer(stage) = continuation.kind() {
            return self
                .should_close_collection_initializer(continuation.first(), stage)
                .map(|close| close.then_some(continuation.first()));
        }
        let NativeContinuationKind::Conversion {
            consumer: ConversionConsumer::BuiltinPropertyKey(consumer),
            ..
        } = continuation.kind()
        else {
            return Ok(None);
        };
        if !matches!(
            consumer,
            BuiltinPropertyKeyConsumer::ObjectFromEntries
                | BuiltinPropertyKeyConsumer::ObjectGroupBy
        ) {
            return Ok(None);
        }
        self.pending_native_property_key(continuation.first())
            .map(|pending| Some(pending.third()))
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
    pub(crate) fn completion_stack_error(error: CompletionStackError) -> ExecutionError {
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
                if instruction.opcode == Opcode::Negate
                    && let Some(bigint) = input.as_small_bigint()
                {
                    let Some(value) = bigint.checked_neg().and_then(Value::from_small_bigint)
                    else {
                        return HotControl::Slow;
                    };
                    registers.write(operands[0], value);
                    return HotControl::Continue;
                }
                if instruction.opcode == Opcode::BitwiseNot
                    && let Some(value) = small_bigint_not_hot(input)
                {
                    registers.write(operands[0], value);
                    return HotControl::Continue;
                }
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
                let Some(value) = numeric_binary_hot(Opcode::Add, left, right)
                    .or_else(|| small_bigint_binary_hot(Opcode::Add, left, right))
                else {
                    return HotControl::Slow;
                };
                registers.write(operands[0], value);
                HotControl::Continue
            }
            Opcode::Sub | Opcode::Mul | Opcode::Div => {
                let left = registers.read(operands[1]);
                let right = registers.read(operands[2]);
                let Some(value) = numeric_binary_hot(instruction.opcode, left, right)
                    .or_else(|| small_bigint_binary_hot(instruction.opcode, left, right))
                else {
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
                if numeric_value(left).is_some() && numeric_value(right).is_some() {
                    registers.write(
                        operands[0],
                        numeric_binary_operation(instruction.opcode, left, right),
                    );
                    return HotControl::Continue;
                }
                let Some(value) = small_bigint_binary_hot(instruction.opcode, left, right) else {
                    return HotControl::Slow;
                };
                registers.write(operands[0], value);
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
