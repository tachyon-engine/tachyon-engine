use super::*;

impl Lowerer<'_> {
    /// Lowers one script statement while preserving the most recent non-empty completion value.
    pub(in crate::bytecode) fn entry_statement(
        &mut self,
        statement: &HirStatement,
        result: RegisterId,
    ) -> Result<bool, CompileError> {
        match &statement.kind {
            HirStatementKind::Expression(expression) => {
                let value = self.expression(expression)?;
                self.emit(
                    Opcode::Move,
                    &[result.index(), value.index()],
                    statement.span,
                )?;
                Ok(false)
            }
            HirStatementKind::VariableDeclaration(declaration) => {
                self.variable_declaration(declaration)?;
                Ok(false)
            }
            HirStatementKind::FunctionDeclaration(_) | HirStatementKind::Empty => Ok(false),
            HirStatementKind::Throw(argument) => {
                let value = self.expression(argument)?;
                self.emit(Opcode::Throw, &[value.index()], statement.span)?;
                Ok(true)
            }
            HirStatementKind::Block(statements) => {
                let checkpoint = self.locals.len();
                let mut terminal = false;
                for statement in statements.iter() {
                    terminal = self.entry_statement(statement, result)?;
                    if terminal {
                        break;
                    }
                }
                self.locals.truncate(checkpoint);
                Ok(terminal)
            }
            HirStatementKind::If {
                test,
                consequent,
                alternate,
            } => self.entry_if_statement(
                test,
                consequent,
                alternate.as_deref(),
                result,
                statement.span,
            ),
            HirStatementKind::For {
                initializer,
                test,
                update,
                body,
            } => {
                self.entry_for_statement(
                    initializer.as_ref(),
                    test.as_ref(),
                    update.as_ref(),
                    body,
                    result,
                    statement.span,
                )?;
                Ok(false)
            }
            HirStatementKind::ForIn { left, right, body } => {
                self.entry_for_in_statement(left, right, body, result, statement.span)?;
                Ok(false)
            }
            HirStatementKind::ForOf { .. } => {
                Err(self.unsupported(statement.span, "for-of bytecode"))
            }
            HirStatementKind::Loop {
                test,
                body,
                test_first,
            } => {
                self.entry_loop_statement(test, body, result, *test_first, statement.span)?;
                Ok(false)
            }
            HirStatementKind::Switch {
                discriminant,
                cases,
            } => {
                self.entry_switch_statement(discriminant, cases, result, statement.span)?;
                Ok(false)
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => self.entry_try_statement(
                block,
                handler.as_ref(),
                finalizer.as_deref(),
                result,
                statement.span,
            ),
            HirStatementKind::Break => {
                let target = self.current_break_target(statement.span)?;
                self.emit_control_jump(target, Opcode::BreakThroughFinally, statement.span)?;
                Ok(true)
            }
            HirStatementKind::Continue => {
                let target = self.current_continue_target(statement.span)?;
                self.emit_control_jump(target, Opcode::ContinueThroughFinally, statement.span)?;
                Ok(true)
            }
            HirStatementKind::Return(_) => Err(CompileError::UnsupportedSyntax {
                source_name: self.source_name.clone(),
                span: statement.span,
                syntax: "top-level return",
            }),
        }
    }

    /// Emits a script conditional and updates the shared completion register only in executed arms.
    pub(in crate::bytecode) fn entry_if_statement(
        &mut self,
        test: &HirExpression,
        consequent: &HirStatement,
        alternate: Option<&HirStatement>,
        result: RegisterId,
        span: SourceSpan,
    ) -> Result<bool, CompileError> {
        let test = self.expression(test)?;
        let alternate_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let end_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let bytecode_span = BytecodeSourceSpan {
            start: span.start,
            end: span.end,
        };
        self.builder
            .emit_jump_if_false(test, alternate_label, bytecode_span)
            .map_err(CompileError::Builder)?;
        let consequent_terminal = self.entry_statement(consequent, result)?;
        self.builder
            .emit_jump(end_label, bytecode_span)
            .map_err(CompileError::Builder)?;
        self.builder
            .bind_label(alternate_label)
            .map_err(CompileError::Builder)?;
        let alternate_terminal = alternate
            .map(|alternate| self.entry_statement(alternate, result))
            .transpose()?
            .unwrap_or(false);
        self.builder
            .bind_label(end_label)
            .map_err(CompileError::Builder)?;
        Ok(alternate.is_some() && consequent_terminal && alternate_terminal)
    }

    /// Emits a classic script for-loop while preserving completion and update-before-continue flow.
    pub(in crate::bytecode) fn entry_for_statement(
        &mut self,
        initializer: Option<&HirForInitializer>,
        test: Option<&HirExpression>,
        update: Option<&HirExpression>,
        body: &HirStatement,
        result: RegisterId,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let checkpoint = self.locals.len();
        self.for_initializer(initializer)?;
        let condition = self.builder.new_label().map_err(CompileError::Builder)?;
        let update_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        self.builder
            .bind_label(condition)
            .map_err(CompileError::Builder)?;
        if let Some(test) = test {
            let test = self.expression(test)?;
            self.builder
                .emit_jump_if_false(
                    test,
                    end,
                    BytecodeSourceSpan {
                        start: span.start,
                        end: span.end,
                    },
                )
                .map_err(CompileError::Builder)?;
        }
        self.break_targets.push(self.control_target(end));
        self.continue_targets
            .push(self.control_target(update_label));
        self.entry_statement(body, result)?;
        self.continue_targets.pop();
        self.break_targets.pop();
        self.builder
            .bind_label(update_label)
            .map_err(CompileError::Builder)?;
        if let Some(update) = update {
            self.discarded_expression(update)?;
        }
        self.emit_jump(condition, span)?;
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        self.restore_for_scope(initializer, checkpoint);
        Ok(())
    }

    /// Emits script `for-in` using one managed key snapshot and the existing completion register.
    pub(in crate::bytecode) fn entry_for_in_statement(
        &mut self,
        left: &HirForInLeft,
        right: &HirExpression,
        body: &HirStatement,
        result: RegisterId,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let checkpoint = self.locals.len();
        let (condition, end) = self.for_in_prelude(left, right, span)?;
        self.break_targets.push(self.control_target(end));
        self.continue_targets.push(self.control_target(condition));
        self.entry_statement(body, result)?;
        self.continue_targets.pop();
        self.break_targets.pop();
        self.emit_jump(condition, span)?;
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        self.restore_for_in_scope(left, checkpoint);
        Ok(())
    }

    /// Emits a script while/do-while loop with the condition as the continue destination.
    pub(in crate::bytecode) fn entry_loop_statement(
        &mut self,
        test: &HirExpression,
        body: &HirStatement,
        result: RegisterId,
        test_first: bool,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let body_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let condition = self.builder.new_label().map_err(CompileError::Builder)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        if test_first {
            self.emit_jump(condition, span)?;
        }
        self.builder
            .bind_label(body_label)
            .map_err(CompileError::Builder)?;
        self.break_targets.push(self.control_target(end));
        self.continue_targets.push(self.control_target(condition));
        self.entry_statement(body, result)?;
        self.continue_targets.pop();
        self.break_targets.pop();
        self.builder
            .bind_label(condition)
            .map_err(CompileError::Builder)?;
        let test = self.expression(test)?;
        self.builder
            .emit_jump_if_true(
                test,
                body_label,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        self.builder.bind_label(end).map_err(CompileError::Builder)
    }

    /// Lowers one function-body statement and reports whether it ends in an abrupt completion.
    pub(in crate::bytecode) fn function_statement(
        &mut self,
        statement: &HirStatement,
    ) -> Result<bool, CompileError> {
        match &statement.kind {
            HirStatementKind::Expression(expression) => {
                self.discarded_expression(expression)?;
                Ok(false)
            }
            HirStatementKind::VariableDeclaration(declaration) => {
                self.variable_declaration(declaration)?;
                Ok(false)
            }
            HirStatementKind::Return(argument) => {
                if let Some(argument) = argument {
                    let value = self.expression(argument)?;
                    self.emit(Opcode::Return, &[value.index()], statement.span)?;
                } else {
                    self.emit(Opcode::ReturnUndefined, &[], statement.span)?;
                }
                Ok(true)
            }
            HirStatementKind::Throw(argument) => {
                let value = self.expression(argument)?;
                self.emit(Opcode::Throw, &[value.index()], statement.span)?;
                Ok(true)
            }
            HirStatementKind::Block(statements) => {
                let checkpoint = self.locals.len();
                let mut terminal = false;
                for statement in statements.iter() {
                    terminal = self.function_statement(statement)?;
                    if terminal {
                        break;
                    }
                }
                self.locals.truncate(checkpoint);
                Ok(terminal)
            }
            HirStatementKind::If {
                test,
                consequent,
                alternate,
            } => {
                self.if_statement(test, consequent, alternate.as_deref(), statement.span)?;
                Ok(false)
            }
            HirStatementKind::For {
                initializer,
                test,
                update,
                body,
            } => {
                self.function_for_statement(
                    initializer.as_ref(),
                    test.as_ref(),
                    update.as_ref(),
                    body,
                    statement.span,
                )?;
                Ok(false)
            }
            HirStatementKind::ForIn { left, right, body } => {
                self.function_for_in_statement(left, right, body, statement.span)?;
                Ok(false)
            }
            HirStatementKind::ForOf { .. } => {
                Err(self.unsupported(statement.span, "for-of bytecode"))
            }
            HirStatementKind::Loop {
                test,
                body,
                test_first,
            } => {
                self.function_loop_statement(test, body, *test_first, statement.span)?;
                Ok(false)
            }
            HirStatementKind::Switch {
                discriminant,
                cases,
            } => {
                self.function_switch_statement(discriminant, cases, statement.span)?;
                Ok(false)
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => self.function_try_statement(
                block,
                handler.as_ref(),
                finalizer.as_deref(),
                statement.span,
            ),
            HirStatementKind::Break => {
                let target = self.current_break_target(statement.span)?;
                self.emit_control_jump(target, Opcode::BreakThroughFinally, statement.span)?;
                Ok(true)
            }
            HirStatementKind::Continue => {
                let target = self.current_continue_target(statement.span)?;
                self.emit_control_jump(target, Opcode::ContinueThroughFinally, statement.span)?;
                Ok(true)
            }
            HirStatementKind::Empty => Ok(false),
            HirStatementKind::FunctionDeclaration(_) => Err(CompileError::UnsupportedSyntax {
                source_name: self.source_name.clone(),
                span: statement.span,
                syntax: "nested function declaration",
            }),
        }
    }

    /// Emits a classic function-body for-loop with explicit break and continue label stacks.
    pub(in crate::bytecode) fn function_for_statement(
        &mut self,
        initializer: Option<&HirForInitializer>,
        test: Option<&HirExpression>,
        update: Option<&HirExpression>,
        body: &HirStatement,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let checkpoint = self.locals.len();
        self.for_initializer(initializer)?;
        let condition = self.builder.new_label().map_err(CompileError::Builder)?;
        let update_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        self.builder
            .bind_label(condition)
            .map_err(CompileError::Builder)?;
        if let Some(test) = test {
            let test = self.expression(test)?;
            self.builder
                .emit_jump_if_false(
                    test,
                    end,
                    BytecodeSourceSpan {
                        start: span.start,
                        end: span.end,
                    },
                )
                .map_err(CompileError::Builder)?;
        }
        self.break_targets.push(self.control_target(end));
        self.continue_targets
            .push(self.control_target(update_label));
        self.function_statement(body)?;
        self.continue_targets.pop();
        self.break_targets.pop();
        self.builder
            .bind_label(update_label)
            .map_err(CompileError::Builder)?;
        if let Some(update) = update {
            self.discarded_expression(update)?;
        }
        self.emit_jump(condition, span)?;
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        self.restore_for_scope(initializer, checkpoint);
        Ok(())
    }

    /// Emits function-body `for-in` without introducing native recursion or iterator host state.
    pub(in crate::bytecode) fn function_for_in_statement(
        &mut self,
        left: &HirForInLeft,
        right: &HirExpression,
        body: &HirStatement,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let checkpoint = self.locals.len();
        let (condition, end) = self.for_in_prelude(left, right, span)?;
        self.break_targets.push(self.control_target(end));
        self.continue_targets.push(self.control_target(condition));
        self.function_statement(body)?;
        self.continue_targets.pop();
        self.break_targets.pop();
        self.emit_jump(condition, span)?;
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        self.restore_for_in_scope(left, checkpoint);
        Ok(())
    }

    /// Evaluates the source once, advances the iterator, checks completion, and stores each key.
    pub(in crate::bytecode) fn for_in_prelude(
        &mut self,
        left: &HirForInLeft,
        right: &HirExpression,
        span: SourceSpan,
    ) -> Result<(Label, Label), CompileError> {
        let source = self.expression(right)?;
        let iterator = self.register()?;
        self.emit(
            Opcode::CreateForInIterator,
            &[iterator.index(), source.index()],
            span,
        )?;
        self.prepare_for_in_binding(left, span)?;
        let key = self.register()?;
        let undefined = self.load_undefined(span)?;
        let complete = self.register()?;
        let condition = self.builder.new_label().map_err(CompileError::Builder)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        self.builder
            .bind_label(condition)
            .map_err(CompileError::Builder)?;
        self.emit(Opcode::ForInNext, &[key.index(), iterator.index()], span)?;
        self.emit(
            Opcode::StrictEqual,
            &[complete.index(), key.index(), undefined.index()],
            span,
        )?;
        self.builder
            .emit_jump_if_true(
                complete,
                end,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        self.store_for_in_left(left, key, span)?;
        Ok((condition, end))
    }

    /// Publishes one lexical head binding before its repeated internal initialization.
    pub(in crate::bytecode) fn prepare_for_in_binding(
        &mut self,
        left: &HirForInLeft,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let HirForInLeft::Variable(declaration) = left else {
            return Ok(());
        };
        if declaration.kind == HirVariableDeclarationKind::Var {
            return Ok(());
        }
        let declarator = declaration
            .declarators
            .first()
            .expect("HIR validates one for-in declarator");
        let initial = self.load_undefined(span)?;
        let binding = self.simple_binding(&declarator.pattern)?.clone();
        self.add_local(
            &binding,
            Some(initial),
            declaration.kind == HirVariableDeclarationKind::Let,
        )
    }

    /// Writes one iterator key into a declaration or re-evaluated assignment reference.
    pub(in crate::bytecode) fn store_for_in_left(
        &mut self,
        left: &HirForInLeft,
        value: RegisterId,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        match left {
            HirForInLeft::Assignment(pattern) => self.assign_pattern(pattern, value, span),
            HirForInLeft::Variable(declaration) => {
                let declarator = declaration
                    .declarators
                    .first()
                    .expect("HIR validates one for-in declarator");
                let binding = self.simple_binding(&declarator.pattern)?.clone();
                if let Some(binding) = self.local_by_id(binding.id).cloned() {
                    return self.write_local(&binding, value, span);
                }
                if self.script_scope && declaration.kind == HirVariableDeclarationKind::Var {
                    let scope_name = self.global_binding(&binding.name, true)?;
                    return self.emit(Opcode::StoreScope, &[value.index(), scope_name], span);
                }
                Err(self.unsupported(span, "uninstantiated for-in binding"))
            }
        }
    }

    /// Evaluates a member reference once per iteration and stores an already computed key value.
    pub(in crate::bytecode) fn assign_existing_value(
        &mut self,
        target: &HirAssignmentTarget,
        value: RegisterId,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        match target {
            HirAssignmentTarget::Identifier(target) => {
                if let Some(binding) = self.local_reference(target).cloned() {
                    if !binding.mutable {
                        return Err(self.unsupported(span, "assignment to immutable local"));
                    }
                    return self.write_local(&binding, value, span);
                }
                if let Some(binding) = self.captured_reference(target)? {
                    if !binding.mutable {
                        return Err(self.unsupported(span, "assignment to immutable capture"));
                    }
                    return self.write_local(&binding, value, span);
                }
                self.require_global_reference(target, span)?;
                let scope_name = self.resolved_global_binding(target, true)?;
                self.emit(
                    Opcode::StoreResolvedScope,
                    &[value.index(), scope_name],
                    span,
                )
            }
            HirAssignmentTarget::StaticMember { object, property } => {
                let receiver = self.expression(object)?;
                let property = self.scope_name(property)?;
                self.emit(
                    Opcode::SetById,
                    &[receiver.index(), value.index(), property],
                    span,
                )
            }
            HirAssignmentTarget::ComputedMember { object, property } => {
                let receiver = self.expression(object)?;
                let property = self.expression(property)?;
                self.prepare_property_key(property, receiver, false, span)?;
                self.emit(
                    Opcode::SetByValue,
                    &[receiver.index(), value.index(), property.index()],
                    span,
                )
            }
        }
    }

    /// Destructures one object assignment target while preserving computed-key evaluation order.
    pub(in crate::bytecode) fn assign_pattern(
        &mut self,
        pattern: &crate::HirPattern,
        value: RegisterId,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        match &pattern.kind {
            crate::HirPatternKind::Assignment(target) => {
                self.assign_existing_value(target, value, span)
            }
            crate::HirPatternKind::Default {
                target,
                initializer,
            } => {
                let value = self.default_pattern_value(value, initializer)?;
                self.assign_pattern(target, value, span)
            }
            crate::HirPatternKind::Object { properties, rest } => {
                if rest.is_some() {
                    return Err(self.unsupported(pattern.span, "object rest bytecode"));
                }
                self.require_object_coercible(value, pattern.span)?;
                for property in properties.iter() {
                    let property_value =
                        self.pattern_property(value, &property.key, property.span)?;
                    self.assign_pattern(&property.target, property_value, span)?;
                }
                Ok(())
            }
            crate::HirPatternKind::Array { elements, rest } => {
                if rest.is_some() {
                    return Err(self.unsupported(pattern.span, "array rest bytecode"));
                }
                let iterator = self.get_sync_iterator(value, pattern.span)?;
                for element in elements.iter() {
                    let next = self.iterator_next(iterator, pattern.span)?;
                    let use_undefined = self.builder.new_label().map_err(CompileError::Builder)?;
                    let end = self.builder.new_label().map_err(CompileError::Builder)?;
                    self.builder
                        .emit_jump_if_true(
                            iterator.done,
                            use_undefined,
                            tachyon_bytecode::SourceSpan {
                                start: pattern.span.start,
                                end: pattern.span.end,
                            },
                        )
                        .map_err(CompileError::Builder)?;
                    let item = self.pattern_property(
                        next,
                        &crate::HirObjectPropertyKey::Static("value".into()),
                        pattern.span,
                    )?;
                    if let Some(element) = element {
                        self.assign_pattern(element, item, span)?;
                    }
                    self.emit_jump(end, span)?;
                    self.builder
                        .bind_label(use_undefined)
                        .map_err(CompileError::Builder)?;
                    if let Some(element) = element {
                        let undefined = self.load_undefined(pattern.span)?;
                        self.assign_pattern(element, undefined, span)?;
                    }
                    self.builder
                        .bind_label(end)
                        .map_err(CompileError::Builder)?;
                }
                self.close_iterator_normally(iterator, pattern.span)
            }
            crate::HirPatternKind::Binding(_) => {
                Err(self.unsupported(pattern.span, "destructuring pattern bytecode"))
            }
        }
    }

    pub(in crate::bytecode) fn restore_for_in_scope(
        &mut self,
        left: &HirForInLeft,
        checkpoint: usize,
    ) {
        if matches!(
            left,
            HirForInLeft::Variable(declaration)
                if declaration.kind != HirVariableDeclarationKind::Var
        ) {
            self.locals.truncate(checkpoint);
        }
    }

    /// Emits an ordinary-function while/do-while loop without entering the Rust call stack.
    pub(in crate::bytecode) fn function_loop_statement(
        &mut self,
        test: &HirExpression,
        body: &HirStatement,
        test_first: bool,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let body_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let condition = self.builder.new_label().map_err(CompileError::Builder)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        if test_first {
            self.emit_jump(condition, span)?;
        }
        self.builder
            .bind_label(body_label)
            .map_err(CompileError::Builder)?;
        self.break_targets.push(self.control_target(end));
        self.continue_targets.push(self.control_target(condition));
        self.function_statement(body)?;
        self.continue_targets.pop();
        self.break_targets.pop();
        self.builder
            .bind_label(condition)
            .map_err(CompileError::Builder)?;
        let test = self.expression(test)?;
        self.builder
            .emit_jump_if_true(
                test,
                body_label,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        self.builder.bind_label(end).map_err(CompileError::Builder)
    }

    pub(in crate::bytecode) fn for_initializer(
        &mut self,
        initializer: Option<&HirForInitializer>,
    ) -> Result<(), CompileError> {
        match initializer {
            Some(HirForInitializer::Variable(declaration)) => {
                self.variable_declaration(declaration)
            }
            Some(HirForInitializer::Expression(expression)) => {
                self.expression(expression).map(|_| ())
            }
            None => Ok(()),
        }
    }

    pub(in crate::bytecode) fn restore_for_scope(
        &mut self,
        initializer: Option<&HirForInitializer>,
        checkpoint: usize,
    ) {
        if let Some(HirForInitializer::Variable(declaration)) = initializer
            && !matches!(declaration.kind, HirVariableDeclarationKind::Var)
        {
            self.locals.truncate(checkpoint);
        }
    }

    /// Emits a structured conditional while leaving both lexical branches to the statement lowerer.
    pub(in crate::bytecode) fn if_statement(
        &mut self,
        test: &HirExpression,
        consequent: &HirStatement,
        alternate: Option<&HirStatement>,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let test = self.expression(test)?;
        let alternate_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let end_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let bytecode_span = BytecodeSourceSpan {
            start: span.start,
            end: span.end,
        };
        self.builder
            .emit_jump_if_false(test, alternate_label, bytecode_span)
            .map_err(CompileError::Builder)?;
        self.function_statement(consequent)?;
        self.builder
            .emit_jump(end_label, bytecode_span)
            .map_err(CompileError::Builder)?;
        self.builder
            .bind_label(alternate_label)
            .map_err(CompileError::Builder)?;
        if let Some(alternate) = alternate {
            self.function_statement(alternate)?;
        }
        self.builder
            .bind_label(end_label)
            .map_err(CompileError::Builder)
    }

    /// Lowers a script try/catch into one immutable range while sharing UpdateEmpty state.
    pub(in crate::bytecode) fn entry_try_statement(
        &mut self,
        block: &[HirStatement],
        handler: Option<&HirCatchClause>,
        finalizer: Option<&[HirStatement]>,
        result: RegisterId,
        span: SourceSpan,
    ) -> Result<bool, CompileError> {
        if finalizer.is_none() {
            let handler = handler.ok_or_else(|| self.unsupported(span, "try without catch"))?;
            return self.entry_try_catch(block, handler, result, span);
        }
        let finalizer = finalizer.expect("checked above");
        let finally_slot = self.reserve_handler();
        let catch_slot = handler.map(|_| self.reserve_handler());
        self.finally_depth =
            self.finally_depth
                .checked_add(1)
                .ok_or(CompileError::LoweringCapacityOverflow {
                    collection: "finally nesting depth",
                })?;
        let protected_start = self.emit_marker(span)?;
        let try_terminal = self.entry_statement_list(block, result)?;
        if !try_terminal {
            self.emit(Opcode::EnterFinally, &[], span)?;
        }
        let (catch_offset, catch_terminal) = if let Some(handler) = handler {
            let checkpoint = self.locals.len();
            let offset = self.emit_catch_binding(handler)?;
            let terminal = self.entry_statement_list(&handler.body, result)?;
            self.locals.truncate(checkpoint);
            if !terminal {
                self.emit(Opcode::EnterFinally, &[], span)?;
            }
            (Some(offset), terminal)
        } else {
            (None, true)
        };
        let finalizer_offset = self
            .builder
            .current_offset()
            .map_err(CompileError::Builder)?;
        let finalizer_result = self.register()?;
        let finalizer_terminal = self.entry_statement_list(finalizer, finalizer_result)?;
        self.emit(Opcode::ResumeCompletion, &[], span)?;
        let handler_end = self
            .builder
            .current_offset()
            .map_err(CompileError::Builder)?;
        self.finally_depth -= 1;
        self.publish_finally_handler(finally_slot, protected_start, finalizer_offset, handler_end)?;
        if let (Some(slot), Some(offset)) = (catch_slot, catch_offset) {
            self.publish_catch_handler(slot, protected_start, offset)?;
        }
        Ok(finalizer_terminal || (try_terminal && catch_terminal))
    }

    /// Lowers an ordinary-function try/catch with identical handler and lexical checkpoints.
    pub(in crate::bytecode) fn function_try_statement(
        &mut self,
        block: &[HirStatement],
        handler: Option<&HirCatchClause>,
        finalizer: Option<&[HirStatement]>,
        span: SourceSpan,
    ) -> Result<bool, CompileError> {
        if finalizer.is_none() {
            let handler = handler.ok_or_else(|| self.unsupported(span, "try without catch"))?;
            return self.function_try_catch(block, handler, span);
        }
        let finalizer = finalizer.expect("checked above");
        let finally_slot = self.reserve_handler();
        let catch_slot = handler.map(|_| self.reserve_handler());
        self.finally_depth =
            self.finally_depth
                .checked_add(1)
                .ok_or(CompileError::LoweringCapacityOverflow {
                    collection: "finally nesting depth",
                })?;
        let protected_start = self.emit_marker(span)?;
        let try_terminal = self.function_statement_list(block)?;
        if !try_terminal {
            self.emit(Opcode::EnterFinally, &[], span)?;
        }
        let (catch_offset, catch_terminal) = if let Some(handler) = handler {
            let checkpoint = self.locals.len();
            let offset = self.emit_catch_binding(handler)?;
            let terminal = self.function_statement_list(&handler.body)?;
            self.locals.truncate(checkpoint);
            if !terminal {
                self.emit(Opcode::EnterFinally, &[], span)?;
            }
            (Some(offset), terminal)
        } else {
            (None, true)
        };
        let finalizer_offset = self
            .builder
            .current_offset()
            .map_err(CompileError::Builder)?;
        let finalizer_terminal = self.function_statement_list(finalizer)?;
        self.emit(Opcode::ResumeCompletion, &[], span)?;
        let handler_end = self
            .builder
            .current_offset()
            .map_err(CompileError::Builder)?;
        self.finally_depth -= 1;
        self.publish_finally_handler(finally_slot, protected_start, finalizer_offset, handler_end)?;
        if let (Some(slot), Some(offset)) = (catch_slot, catch_offset) {
            self.publish_catch_handler(slot, protected_start, offset)?;
        }
        let terminal = finalizer_terminal || (try_terminal && catch_terminal);
        if terminal {
            self.emit(Opcode::ReturnUndefined, &[], span)?;
        }
        Ok(terminal)
    }

    /// Lowers the established script try/catch shape without a completion record.
    fn entry_try_catch(
        &mut self,
        block: &[HirStatement],
        handler: &HirCatchClause,
        result: RegisterId,
        span: SourceSpan,
    ) -> Result<bool, CompileError> {
        let handler_slot = self.reserve_handler();
        let protected_start = self.emit_marker(span)?;
        let try_terminal = self.entry_statement_list(block, result)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        if !try_terminal {
            self.emit_jump(end, span)?;
        }
        let checkpoint = self.locals.len();
        let handler_offset = self.emit_catch_binding(handler)?;
        let catch_terminal = self.entry_statement_list(&handler.body, result)?;
        self.locals.truncate(checkpoint);
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        self.publish_catch_handler(handler_slot, protected_start, handler_offset)?;
        Ok(try_terminal && catch_terminal)
    }

    /// Lowers the established function try/catch shape without a completion record.
    fn function_try_catch(
        &mut self,
        block: &[HirStatement],
        handler: &HirCatchClause,
        span: SourceSpan,
    ) -> Result<bool, CompileError> {
        let handler_slot = self.reserve_handler();
        let protected_start = self.emit_marker(span)?;
        let try_terminal = self.function_statement_list(block)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        if !try_terminal {
            self.emit_jump(end, span)?;
        }
        let checkpoint = self.locals.len();
        let handler_offset = self.emit_catch_binding(handler)?;
        let catch_terminal = self.function_statement_list(&handler.body)?;
        self.locals.truncate(checkpoint);
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        self.publish_catch_handler(handler_slot, protected_start, handler_offset)?;
        Ok(try_terminal && catch_terminal)
    }

    pub(in crate::bytecode) fn entry_statement_list(
        &mut self,
        statements: &[HirStatement],
        result: RegisterId,
    ) -> Result<bool, CompileError> {
        let checkpoint = self.locals.len();
        let mut terminal = false;
        for statement in statements {
            terminal = self.entry_statement(statement, result)?;
            if terminal {
                break;
            }
        }
        self.locals.truncate(checkpoint);
        Ok(terminal)
    }

    pub(in crate::bytecode) fn function_statement_list(
        &mut self,
        statements: &[HirStatement],
    ) -> Result<bool, CompileError> {
        let checkpoint = self.locals.len();
        let mut terminal = false;
        for statement in statements {
            terminal = self.function_statement(statement)?;
            if terminal {
                break;
            }
        }
        self.locals.truncate(checkpoint);
        Ok(terminal)
    }

    /// Emits the handler entry and optionally binds its pending exception register lexically.
    pub(in crate::bytecode) fn emit_catch_binding(
        &mut self,
        handler: &HirCatchClause,
    ) -> Result<tachyon_bytecode::WordOffset, CompileError> {
        let exception = self.register()?;
        let offset = self
            .builder
            .emit(
                Opcode::LoadException,
                &[exception.index()],
                BytecodeSourceSpan {
                    start: handler.span.start,
                    end: handler.span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        if let Some(parameter) = &handler.parameter {
            let binding = self.simple_binding(parameter)?.clone();
            self.add_local(&binding, Some(exception), true)?;
        }
        Ok(offset)
    }

    pub(in crate::bytecode) fn reserve_handler(&mut self) -> usize {
        let index = self.handlers.len();
        self.handlers.push(None);
        index
    }

    pub(in crate::bytecode) fn emit_marker(
        &mut self,
        span: SourceSpan,
    ) -> Result<tachyon_bytecode::WordOffset, CompileError> {
        self.builder
            .emit(
                Opcode::Nop,
                &[],
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map_err(CompileError::Builder)
    }

    pub(in crate::bytecode) fn publish_catch_handler(
        &mut self,
        slot: usize,
        protected_start: tachyon_bytecode::WordOffset,
        handler: tachyon_bytecode::WordOffset,
    ) -> Result<(), CompileError> {
        let entry = HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            handler_end: handler,
            kind: HandlerKind::Catch,
            environment_depth: 0,
        };
        *self
            .handlers
            .get_mut(slot)
            .ok_or(CompileError::UnboundExceptionHandler)? = Some(entry);
        Ok(())
    }

    /// Publishes one finalizer's protected range and verified execution boundary.
    pub(in crate::bytecode) fn publish_finally_handler(
        &mut self,
        slot: usize,
        protected_start: tachyon_bytecode::WordOffset,
        handler: tachyon_bytecode::WordOffset,
        handler_end: tachyon_bytecode::WordOffset,
    ) -> Result<(), CompileError> {
        let entry = HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            handler_end,
            kind: HandlerKind::Finally,
            environment_depth: 0,
        };
        *self
            .handlers
            .get_mut(slot)
            .ok_or(CompileError::UnboundExceptionHandler)? = Some(entry);
        Ok(())
    }

    pub(in crate::bytecode) fn unsupported(
        &self,
        span: SourceSpan,
        syntax: &'static str,
    ) -> CompileError {
        CompileError::UnsupportedSyntax {
            source_name: self.source_name.clone(),
            span,
            syntax,
        }
    }

    /// Emits switch dispatch and script clause bodies while preserving UpdateEmpty completion state.
    pub(in crate::bytecode) fn entry_switch_statement(
        &mut self,
        discriminant: &HirExpression,
        cases: &[HirSwitchCase],
        result: RegisterId,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let checkpoint = self.locals.len();
        let (case_labels, end) = self.emit_switch_dispatch(discriminant, cases, span)?;
        self.break_targets.push(self.control_target(end));
        for (case, label) in cases.iter().zip(case_labels) {
            self.builder
                .bind_label(label)
                .map_err(CompileError::Builder)?;
            for statement in case.consequent.iter() {
                if self.entry_statement(statement, result)? {
                    break;
                }
            }
        }
        self.break_targets.pop();
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        self.locals.truncate(checkpoint);
        Ok(())
    }

    /// Emits switch dispatch and ordinary-function clause bodies with source-order fallthrough.
    pub(in crate::bytecode) fn function_switch_statement(
        &mut self,
        discriminant: &HirExpression,
        cases: &[HirSwitchCase],
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let checkpoint = self.locals.len();
        let (case_labels, end) = self.emit_switch_dispatch(discriminant, cases, span)?;
        self.break_targets.push(self.control_target(end));
        for (case, label) in cases.iter().zip(case_labels) {
            self.builder
                .bind_label(label)
                .map_err(CompileError::Builder)?;
            for statement in case.consequent.iter() {
                if self.function_statement(statement)? {
                    break;
                }
            }
        }
        self.break_targets.pop();
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        self.locals.truncate(checkpoint);
        Ok(())
    }

    /// Evaluates case tests in source order and returns exact labels for contiguous clause bodies.
    pub(in crate::bytecode) fn emit_switch_dispatch(
        &mut self,
        discriminant: &HirExpression,
        cases: &[HirSwitchCase],
        span: SourceSpan,
    ) -> Result<(Vec<Label>, Label), CompileError> {
        let discriminant_value = self.expression(discriminant)?;
        let discriminant = self.register()?;
        self.emit(
            Opcode::Move,
            &[discriminant.index(), discriminant_value.index()],
            span,
        )?;
        let mut labels = Vec::with_capacity(cases.len());
        for _ in cases {
            labels.push(self.builder.new_label().map_err(CompileError::Builder)?);
        }
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        let mut default = None;
        for (case, label) in cases.iter().zip(labels.iter().copied()) {
            let Some(test) = case.test.as_ref() else {
                default = Some(label);
                continue;
            };
            let test = self.expression(test)?;
            let equal = self.register()?;
            self.emit(
                Opcode::StrictEqual,
                &[equal.index(), discriminant.index(), test.index()],
                case.span,
            )?;
            self.builder
                .emit_jump_if_true(
                    equal,
                    label,
                    BytecodeSourceSpan {
                        start: case.span.start,
                        end: case.span.end,
                    },
                )
                .map_err(CompileError::Builder)?;
        }
        self.emit_jump(default.unwrap_or(end), span)?;
        Ok((labels, end))
    }
}
