mod array_accumulation;
mod expression;
mod statement;

use tachyon_bytecode::{
    BindingLocation, BindingPlanEntry, BytecodeBuilder, BytecodeConstant, FunctionId, HandlerEntry,
    HandlerKind, Label, Opcode, RegisterId, SourceSpan as BytecodeSourceSpan, SuspendPoint,
    SuspendPointId,
};

use crate::hir::{HirArrayExpressionPart, HirObjectExpressionPart};
use crate::hir::{
    HirAssignmentOperator, HirAssignmentTarget, HirForInLeft, HirPattern, HirPatternKind,
};
use crate::{
    BindingId, CompileError, HirBinaryOperator, HirCatchClause, HirExpression, HirExpressionKind,
    HirForInitializer, HirFunctionDeclaration, HirIdentifierReference, HirLogicalOperator,
    HirObjectPropertyKey, HirObjectPropertyValue, HirStatement, HirStatementKind, HirSwitchCase,
    HirUnaryOperator, HirUpdateOperator, HirVariableDeclaration, HirVariableDeclarationKind,
    ScopeId, SourceName, SourceSpan,
};

use super::{EnvironmentPlans, GlobalLexicalPlan};

pub(super) struct Lowerer<'a> {
    pub(super) builder: BytecodeBuilder,
    pub(super) constants: &'a mut Vec<BytecodeConstant>,
    pub(super) scope_names: &'a mut Vec<std::sync::Arc<str>>,
    pub(super) locals: Vec<LocalBinding>,
    pub(super) binding_plan: Vec<BindingPlanEntry>,
    pub(super) break_targets: Vec<ControlTarget>,
    pub(super) continue_targets: Vec<ControlTarget>,
    pub(super) pending_loop_labels: Vec<std::sync::Arc<str>>,
    pub(super) handlers: Vec<Option<HandlerEntry>>,
    pub(super) suspend_points: Vec<SuspendPoint>,
    pub(super) finally_depth: u32,
    /// Number of dynamically-entered lexical environments in the current function.
    pub(super) environment_depth: u32,
    pub(super) next_register: u32,
    pub(super) source_name: SourceName,
    pub(super) script_scope: bool,
    pub(super) module_scope: bool,
    pub(super) root_scope: ScopeId,
    pub(super) function_scope: Option<ScopeId>,
    pub(super) is_arrow: bool,
    /// Whether `yield` lowering must use the async-generator protocol.
    pub(super) is_async_generator: bool,
    pub(super) strict: bool,
    pub(super) initialize_instance_elements: bool,
    /// Whether this function may replace its frame at strict tail-call sites.
    pub(super) proper_tail_calls: bool,
    /// Whether the activation must preserve the original argument sequence after entry.
    pub(super) needs_argument_source: bool,
    /// Semantic scope matching the environment currently exposed to emitted loads.
    pub(super) active_scope: ScopeId,
    pub(super) environments: &'a EnvironmentPlans,
}

#[derive(Clone, Debug)]
pub(super) struct ControlTarget {
    label: Label,
    finally_depth: u32,
    name: Option<std::sync::Arc<str>>,
}

#[derive(Clone, Debug)]
pub(super) struct LocalBinding {
    id: BindingId,
    storage: LocalStorage,
    mutable: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum LocalStorage {
    Register(RegisterId),
    Environment { depth: u32, slot: u32 },
}

/// Registers retained for one synchronous iterator record across lowered pattern operations.
#[derive(Clone, Copy, Debug)]
pub(super) struct IteratorRegisters {
    pub(super) iterator: RegisterId,
    pub(super) receiver: RegisterId,
    pub(super) next: RegisterId,
    done: RegisterId,
}

impl Lowerer<'_> {
    /// Identifies the current function's implicit arguments binding ahead of outer bindings.
    pub(super) fn is_implicit_arguments_reference(
        &self,
        reference: &HirIdentifierReference,
    ) -> bool {
        reference.name.as_ref() == "arguments"
            && !self.is_arrow
            && self
                .function_scope
                .is_some_and(|scope| reference.binding_scope != Some(scope))
    }

    /// Destructures one object binding into locals, preserving property and default evaluation order.
    pub(super) fn bind_pattern(
        &mut self,
        pattern: &HirPattern,
        value: RegisterId,
        mutable: bool,
    ) -> Result<(), CompileError> {
        match &pattern.kind {
            HirPatternKind::Binding(binding) => {
                if let Some(local) = self.local_by_id(binding.id).cloned() {
                    return self.initialize_local(&local, value, pattern.span);
                }
                self.add_local(binding, Some(value), mutable)
            }
            HirPatternKind::Default {
                target,
                initializer,
            } => {
                let value =
                    self.default_pattern_value(value, initializer, target.inferred_name())?;
                self.bind_pattern(target, value, mutable)
            }
            HirPatternKind::Object { properties, rest } => {
                self.require_object_coercible(value, pattern.span)?;
                let exclusions = rest
                    .as_ref()
                    .map(|_| self.create_exclusion_list(properties.len(), pattern.span))
                    .transpose()?;
                for property in properties.iter() {
                    let (property_value, key) =
                        self.pattern_property_with_key(value, &property.key, property.span)?;
                    if let Some(exclusions) = exclusions {
                        self.exclude_pattern_key(exclusions, key, property.span)?;
                    }
                    self.bind_pattern(&property.target, property_value, mutable)?;
                }
                if let Some(rest) = rest {
                    let target = self.register()?;
                    self.emit(Opcode::CreateObject, &[target.index()], pattern.span)?;
                    self.emit(
                        Opcode::CopyDataProperties,
                        &[
                            target.index(),
                            value.index(),
                            exclusions.expect("rest allocates exclusions").index(),
                        ],
                        pattern.span,
                    )?;
                    self.bind_pattern(rest, target, mutable)?;
                }
                Ok(())
            }
            HirPatternKind::Array { elements, rest } => {
                let iterator = self.get_sync_iterator(value, pattern.span)?;
                for element in elements.iter() {
                    let next = self.iterator_next(iterator, pattern.span)?;
                    let use_undefined = self.builder.new_label().map_err(CompileError::Builder)?;
                    let end = self.builder.new_label().map_err(CompileError::Builder)?;
                    let selected = element.as_ref().map(|_| self.register()).transpose()?;
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
                    if let Some(selected) = selected {
                        let item = self.pattern_property(
                            next,
                            &HirObjectPropertyKey::Static("value".into()),
                            pattern.span,
                        )?;
                        self.emit(
                            Opcode::Move,
                            &[selected.index(), item.index()],
                            pattern.span,
                        )?;
                    }
                    self.emit_jump(end, pattern.span)?;
                    self.builder
                        .bind_label(use_undefined)
                        .map_err(CompileError::Builder)?;
                    if let Some(selected) = selected {
                        let undefined = self.load_undefined(pattern.span)?;
                        self.emit(
                            Opcode::Move,
                            &[selected.index(), undefined.index()],
                            pattern.span,
                        )?;
                    }
                    self.builder
                        .bind_label(end)
                        .map_err(CompileError::Builder)?;
                    if let (Some(element), Some(selected)) = (element, selected) {
                        self.bind_pattern(element, selected, mutable)?;
                    }
                }
                if let Some(rest) = rest {
                    let array = self.collect_iterator_rest(iterator, pattern.span)?;
                    self.bind_pattern(rest, array, mutable)?;
                }
                self.close_iterator_normally(iterator, pattern.span)
            }
            HirPatternKind::Assignment(_) => {
                Err(self.unsupported(pattern.span, "destructuring pattern bytecode"))
            }
        }
    }

    /// Obtains and caches a synchronous iterator record through the realm's `Symbol.iterator`.
    pub(super) fn get_sync_iterator(
        &mut self,
        object: RegisterId,
        span: SourceSpan,
    ) -> Result<IteratorRegisters, CompileError> {
        let iterator_key = self.register()?;
        self.emit(Opcode::LoadIteratorSymbol, &[iterator_key.index()], span)?;
        self.prepare_property_key(iterator_key, object, false, span)?;
        let iterator = self.computed_method_call(object, iterator_key, span)?;
        self.emit(Opcode::CheckObject, &[iterator.index()], span)?;
        let receiver = self.register()?;
        self.emit(Opcode::Move, &[receiver.index(), iterator.index()], span)?;
        let next = self.register()?;
        let next_atom = self.scope_name(&std::sync::Arc::from("next"))?;
        self.emit(
            Opcode::GetById,
            &[next.index(), receiver.index(), next_atom],
            span,
        )?;
        let done = self.load_boolean(false, span)?;
        Ok(IteratorRegisters {
            iterator,
            receiver,
            next,
            done,
        })
    }

    /// Selects an async iterator or a direct Async-from-Sync record before the loop starts.
    pub(super) fn get_async_or_sync_iterator(
        &mut self,
        object: RegisterId,
        span: SourceSpan,
    ) -> Result<IteratorRegisters, CompileError> {
        let iterator = IteratorRegisters {
            iterator: self.register()?,
            receiver: self.register()?,
            next: self.register()?,
            done: self.load_boolean(false, span)?,
        };
        let sync_setup = self.builder.new_label().map_err(CompileError::Builder)?;
        let setup_end = self.builder.new_label().map_err(CompileError::Builder)?;
        let iterator_key = self.register()?;
        self.emit(
            Opcode::LoadAsyncIteratorSymbol,
            &[iterator_key.index()],
            span,
        )?;
        self.prepare_property_key(iterator_key, object, false, span)?;
        let async_receiver = self.register()?;
        self.emit(
            Opcode::Move,
            &[async_receiver.index(), object.index()],
            span,
        )?;
        let async_method = self.register()?;
        self.emit(
            Opcode::GetByValue,
            &[
                async_method.index(),
                async_receiver.index(),
                iterator_key.index(),
            ],
            span,
        )?;
        let undefined = self.load_undefined(span)?;
        let missing = self.register()?;
        self.emit(
            Opcode::StrictEqual,
            &[missing.index(), async_method.index(), undefined.index()],
            span,
        )?;
        self.builder
            .emit_jump_if_true(
                missing,
                sync_setup,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        let null = self.load_null(span)?;
        self.emit(
            Opcode::StrictEqual,
            &[missing.index(), async_method.index(), null.index()],
            span,
        )?;
        self.builder
            .emit_jump_if_true(
                missing,
                sync_setup,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        let async_iterator = self.call_receiver(async_receiver, async_method, span)?;
        self.emit(Opcode::CheckObject, &[async_iterator.index()], span)?;
        self.emit(
            Opcode::Move,
            &[iterator.iterator.index(), async_iterator.index()],
            span,
        )?;
        self.emit(
            Opcode::Move,
            &[iterator.receiver.index(), async_iterator.index()],
            span,
        )?;
        let next_atom = self.scope_name(&std::sync::Arc::from("next"))?;
        self.emit(
            Opcode::GetById,
            &[iterator.next.index(), iterator.receiver.index(), next_atom],
            span,
        )?;
        self.emit_jump(setup_end, span)?;
        self.builder
            .bind_label(sync_setup)
            .map_err(CompileError::Builder)?;
        let sync_iterator = self.get_sync_iterator(object, span)?;
        let wrapper = self.register()?;
        self.emit(
            Opcode::CreateAsyncFromSyncIterator,
            &[
                wrapper.index(),
                sync_iterator.iterator.index(),
                sync_iterator.next.index(),
            ],
            span,
        )?;
        self.emit(
            Opcode::Move,
            &[iterator.iterator.index(), wrapper.index()],
            span,
        )?;
        self.emit(
            Opcode::Move,
            &[iterator.receiver.index(), wrapper.index()],
            span,
        )?;
        self.emit(
            Opcode::GetById,
            &[iterator.next.index(), iterator.receiver.index(), next_atom],
            span,
        )?;
        self.builder
            .bind_label(setup_end)
            .map_err(CompileError::Builder)?;
        Ok(iterator)
    }

    /// Calls cached `next`, then updates the record's done register from its result object.
    pub(super) fn iterator_next(
        &mut self,
        iterator: IteratorRegisters,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let result = self.call_receiver(iterator.receiver, iterator.next, span)?;
        self.emit(Opcode::CheckObject, &[result.index()], span)?;
        let done =
            self.pattern_property(result, &HirObjectPropertyKey::Static("done".into()), span)?;
        self.emit(Opcode::Move, &[iterator.done.index(), done.index()], span)?;
        Ok(result)
    }

    /// Drains every remaining synchronous iterator value into a fresh Array in source order.
    pub(super) fn collect_iterator_rest(
        &mut self,
        iterator: IteratorRegisters,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let array = self.register()?;
        self.emit(Opcode::CreateArray, &[array.index()], span)?;
        let index = self.load_immediate(0, span)?;
        let loop_start = self.builder.new_label().map_err(CompileError::Builder)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        self.builder
            .bind_label(loop_start)
            .map_err(CompileError::Builder)?;
        let next = self.iterator_next(iterator, span)?;
        self.builder
            .emit_jump_if_true(
                iterator.done,
                end,
                tachyon_bytecode::SourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        let item =
            self.pattern_property(next, &HirObjectPropertyKey::Static("value".into()), span)?;
        self.emit(
            Opcode::SetByValue,
            &[array.index(), item.index(), index.index()],
            span,
        )?;
        let one = self.load_immediate(1, span)?;
        let next_index = self.emit_binary(HirBinaryOperator::Add, index, one, span)?;
        self.emit(Opcode::Move, &[index.index(), next_index.index()], span)?;
        self.emit_jump(loop_start, span)?;
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        Ok(array)
    }

    /// Loads a computed method then calls it with its original receiver in a contiguous window.
    fn computed_method_call(
        &mut self,
        receiver: RegisterId,
        key: RegisterId,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let base = self.register()?;
        self.emit(Opcode::Move, &[base.index(), receiver.index()], span)?;
        let callee = self.register()?;
        self.emit(
            Opcode::GetByValue,
            &[callee.index(), base.index(), key.index()],
            span,
        )?;
        self.call_receiver(base, callee, span)
    }

    /// Calls a cached callee located immediately after the receiver register.
    fn call_receiver(
        &mut self,
        receiver: RegisterId,
        callee: RegisterId,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        debug_assert_eq!(callee.index(), receiver.index() + 1);
        let destination = self.register()?;
        self.emit(
            Opcode::CallWithReceiver,
            &[destination.index(), receiver.index(), 0],
            span,
        )?;
        Ok(destination)
    }

    /// Performs the normal-completion `IteratorClose` branch after a pattern stops early.
    fn close_iterator_normally(
        &mut self,
        iterator: IteratorRegisters,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let closed = self.builder.new_label().map_err(CompileError::Builder)?;
        self.builder
            .emit_jump_if_true(
                iterator.done,
                closed,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        let return_value = self.register()?;
        let return_atom = self.scope_name(&std::sync::Arc::from("return"))?;
        self.emit(
            Opcode::GetById,
            &[return_value.index(), iterator.iterator.index(), return_atom],
            span,
        )?;
        let undefined = self.load_undefined(span)?;
        let missing = self.register()?;
        self.emit(
            Opcode::StrictEqual,
            &[missing.index(), return_value.index(), undefined.index()],
            span,
        )?;
        self.builder
            .emit_jump_if_true(
                missing,
                closed,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        let receiver = self.register()?;
        self.emit(
            Opcode::Move,
            &[receiver.index(), iterator.iterator.index()],
            span,
        )?;
        let callee = self.register()?;
        self.emit(Opcode::Move, &[callee.index(), return_value.index()], span)?;
        self.call_receiver(receiver, callee, span)?;
        self.builder
            .bind_label(closed)
            .map_err(CompileError::Builder)
    }

    /// Emits AsyncIteratorClose inside a completion-preserving bytecode finalizer.
    fn close_async_iterator_normally(
        &mut self,
        iterator: IteratorRegisters,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let closed = self.builder.new_label().map_err(CompileError::Builder)?;
        self.builder
            .emit_jump_if_true(
                iterator.done,
                closed,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        let return_value = self.register()?;
        let return_atom = self.scope_name(&std::sync::Arc::from("return"))?;
        self.emit(
            Opcode::GetById,
            &[return_value.index(), iterator.iterator.index(), return_atom],
            span,
        )?;
        let undefined = self.load_undefined(span)?;
        let missing = self.register()?;
        self.emit(
            Opcode::StrictEqual,
            &[missing.index(), return_value.index(), undefined.index()],
            span,
        )?;
        self.builder
            .emit_jump_if_true(
                missing,
                closed,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        let null = self.load_null(span)?;
        self.emit(
            Opcode::StrictEqual,
            &[missing.index(), return_value.index(), null.index()],
            span,
        )?;
        self.builder
            .emit_jump_if_true(
                missing,
                closed,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        let receiver = self.register()?;
        self.emit(
            Opcode::Move,
            &[receiver.index(), iterator.iterator.index()],
            span,
        )?;
        let callee = self.register()?;
        self.emit(Opcode::Move, &[callee.index(), return_value.index()], span)?;
        let promise = self.call_receiver(receiver, callee, span)?;
        let result = self.register()?;
        self.emit_suspend(Opcode::Await, promise, result, span)?;
        self.emit(Opcode::CheckObject, &[result.index()], span)?;
        self.builder
            .bind_label(closed)
            .map_err(CompileError::Builder)
    }

    /// Emits the existing base guard without performing an observable property lookup.
    fn require_object_coercible(
        &mut self,
        value: RegisterId,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let key = self.load_undefined(span)?;
        let prepared = self.register()?;
        self.emit(
            Opcode::ToPropertyKey,
            &[prepared.index(), key.index(), value.index()],
            span,
        )
    }

    /// Reads one object pattern property using the canonical static or computed property opcode.
    pub(super) fn pattern_property(
        &mut self,
        object: RegisterId,
        key: &HirObjectPropertyKey,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        self.pattern_property_with_key(object, key, span)
            .map(|(value, _)| value)
    }

    /// Reads one pattern property and preserves the exact normalized key for object-rest exclusion.
    pub(super) fn pattern_property_with_key(
        &mut self,
        object: RegisterId,
        key: &HirObjectPropertyKey,
        span: SourceSpan,
    ) -> Result<(RegisterId, RegisterId), CompileError> {
        let destination = self.register()?;
        match key {
            HirObjectPropertyKey::Static(property) => {
                let scope_name = self.scope_name(property)?;
                self.emit(
                    Opcode::GetById,
                    &[destination.index(), object.index(), scope_name],
                    span,
                )?;
                let key = self.load_pattern_static_key(property, span)?;
                Ok((destination, key))
            }
            HirObjectPropertyKey::Computed(expression) => {
                let property = self.expression(expression)?;
                self.prepare_property_key(property, object, false, span)?;
                self.emit(
                    Opcode::GetByValue,
                    &[destination.index(), object.index(), property.index()],
                    span,
                )?;
                Ok((destination, property))
            }
        }
    }

    /// Creates the string Value needed only by VM-private object-rest exclusion storage.
    fn load_pattern_static_key(
        &mut self,
        key: &std::sync::Arc<str>,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let unit_count = key.encode_utf16().count();
        let mut units = Vec::new();
        units
            .try_reserve_exact(unit_count)
            .map_err(|_| CompileError::ConstantAllocationFailed)?;
        units.extend(key.encode_utf16());
        let constant =
            u32::try_from(self.constants.len()).map_err(|_| CompileError::ConstantOverflow)?;
        self.constants
            .push(BytecodeConstant::string_from_utf16(units));
        let destination = self.register()?;
        self.emit(Opcode::LoadConstant, &[destination.index(), constant], span)?;
        Ok(destination)
    }

    /// Emits the non-observable exclusion-list instructions used only by object-rest patterns.
    pub(super) fn create_exclusion_list(
        &mut self,
        property_count: usize,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let count = u32::try_from(property_count).map_err(|_| CompileError::RegisterOverflow)?;
        let destination = self.register()?;
        self.emit(
            Opcode::CreateExclusionList,
            &[destination.index(), count],
            span,
        )?;
        Ok(destination)
    }

    /// Adds one already-normalized object-pattern key without re-evaluating computed expressions.
    pub(super) fn exclude_pattern_key(
        &mut self,
        exclusions: RegisterId,
        key: RegisterId,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        self.emit(
            Opcode::ExcludePropertyKey,
            &[exclusions.index(), key.index()],
            span,
        )
    }

    /// Selects an object default initializer without changing the destination register identity.
    pub(super) fn default_pattern_value(
        &mut self,
        value: RegisterId,
        initializer: &HirExpression,
        inferred_name: Option<&std::sync::Arc<str>>,
    ) -> Result<RegisterId, CompileError> {
        let undefined = self.load_undefined(initializer.span)?;
        let is_undefined = self.register()?;
        self.emit(
            Opcode::StrictEqual,
            &[is_undefined.index(), value.index(), undefined.index()],
            initializer.span,
        )?;
        let use_initializer = self.builder.new_label().map_err(CompileError::Builder)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        self.builder
            .emit_jump_if_true(
                is_undefined,
                use_initializer,
                tachyon_bytecode::SourceSpan {
                    start: initializer.span.start,
                    end: initializer.span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        let result = self.register()?;
        self.emit(
            Opcode::Move,
            &[result.index(), value.index()],
            initializer.span,
        )?;
        self.emit_jump(end, initializer.span)?;
        self.builder
            .bind_label(use_initializer)
            .map_err(CompileError::Builder)?;
        let initialized = self.expression(initializer)?;
        let anonymous = match &initializer.kind {
            HirExpressionKind::Function(function) => function.anonymous,
            HirExpressionKind::Class(class) => class.name.is_none(),
            _ => false,
        };
        if anonymous && let Some(name) = inferred_name {
            let name = self.scope_name(name)?;
            self.emit(
                Opcode::SetFunctionName,
                &[initialized.index(), name],
                initializer.span,
            )?;
        }
        self.emit(
            Opcode::Move,
            &[result.index(), initialized.index()],
            initializer.span,
        )?;
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        Ok(result)
    }

    /// Allocates a fresh register and emits one instruction with the HIR span copied into bytecode source metadata.
    pub(super) fn emit(
        &mut self,
        opcode: Opcode,
        operands: &[u32],
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        self.builder
            .emit(
                opcode,
                operands,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map(|_| ())
            .map_err(CompileError::Builder)
    }

    /// Publishes one hoisted function binding before any top-level statement can reference it.
    pub(super) fn function_declaration(
        &mut self,
        declaration: &HirFunctionDeclaration,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let register = self.register()?;
        let function = declaration
            .function
            .index()
            .checked_add(1)
            .ok_or(CompileError::RegisterOverflow)?;
        let scope_name = self.global_binding(&declaration.binding.name, true)?;
        self.emit(Opcode::DeclareScope, &[scope_name], span)?;
        self.emit(Opcode::CreateClosure, &[register.index(), function], span)?;
        self.emit(Opcode::StoreScope, &[register.index(), scope_name], span)?;
        Ok(())
    }

    /// Initializes a pre-instantiated module function cell before source-order evaluation begins.
    pub(super) fn module_function_declaration(
        &mut self,
        declaration: &HirFunctionDeclaration,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let register = self.register()?;
        let function = declaration
            .function
            .index()
            .checked_add(1)
            .ok_or(CompileError::RegisterOverflow)?;
        let binding = self
            .local_by_id(declaration.binding.id)
            .cloned()
            .ok_or(CompileError::BindingOverflow)?;
        let function_id = FunctionId::new(function);
        self.mark_module_function(&binding, &declaration.binding.name, function_id)?;
        self.emit(Opcode::CreateClosure, &[register.index(), function], span)?;
        self.initialize_local(&binding, register, span)
    }

    /// Marks one module binding for SCC-wide function declaration instantiation.
    fn mark_module_function(
        &mut self,
        binding: &LocalBinding,
        name: &std::sync::Arc<str>,
        function: FunctionId,
    ) -> Result<(), CompileError> {
        let LocalStorage::Environment { depth: 0, slot } = binding.storage else {
            return Err(CompileError::BindingOverflow);
        };
        let entry = self
            .binding_plan
            .iter_mut()
            .find(|entry| &entry.name == name);
        let Some(entry) = entry else {
            return Err(CompileError::BindingOverflow);
        };
        entry.location = BindingLocation::ModuleFunction { slot, function };
        Ok(())
    }

    /// Instantiates one direct function-body declaration before ordinary statement execution.
    pub(super) fn local_function_declaration(
        &mut self,
        declaration: &HirFunctionDeclaration,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let register = self.register()?;
        let function = declaration
            .function
            .index()
            .checked_add(1)
            .ok_or(CompileError::RegisterOverflow)?;
        self.emit(Opcode::CreateClosure, &[register.index(), function], span)?;
        self.add_local(&declaration.binding, Some(register), true)
    }

    /// Emits one parameter default prologue, applying it only when the argument is undefined.
    pub(super) fn parameter_initializer(
        &mut self,
        parameter: RegisterId,
        initializer: &HirExpression,
    ) -> Result<(), CompileError> {
        let undefined = self.load_undefined(initializer.span)?;
        let is_undefined = self.register()?;
        self.emit(
            Opcode::StrictEqual,
            &[is_undefined.index(), parameter.index(), undefined.index()],
            initializer.span,
        )?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        self.builder
            .emit_jump_if_false(
                is_undefined,
                end,
                BytecodeSourceSpan {
                    start: initializer.span.start,
                    end: initializer.span.end,
                },
            )
            .map_err(CompileError::Builder)?;
        let value = self.expression(initializer)?;
        self.emit(
            Opcode::Move,
            &[parameter.index(), value.index()],
            initializer.span,
        )?;
        self.builder.bind_label(end).map_err(CompileError::Builder)
    }
}

impl Lowerer<'_> {
    /// Lowers one declaration list in source order so initializers can use preceding local bindings.
    pub(super) fn variable_declaration(
        &mut self,
        declaration: &HirVariableDeclaration,
    ) -> Result<(), CompileError> {
        if declaration.kind == HirVariableDeclarationKind::Var {
            return self.var_initializers(declaration);
        }
        if !matches!(
            declaration.kind,
            HirVariableDeclarationKind::Let | HirVariableDeclarationKind::Const
        ) {
            return Err(CompileError::UnsupportedSyntax {
                source_name: self.source_name.clone(),
                span: declaration
                    .declarators
                    .first()
                    .map_or(SourceSpan { start: 0, end: 0 }, |declarator| {
                        declarator.span
                    }),
                syntax: "variable declaration kind",
            });
        }
        for declarator in declaration.declarators.iter() {
            let binding = declarator.pattern.binding().cloned();
            let register = match declarator.initializer.as_ref() {
                Some(initializer) => self.expression(initializer)?,
                None if declaration.kind == HirVariableDeclarationKind::Let => {
                    self.load_undefined(declarator.span)?
                }
                None => {
                    return Err(CompileError::UnsupportedSyntax {
                        source_name: self.source_name.clone(),
                        span: declarator.span,
                        syntax: "variable declaration without initializer",
                    });
                }
            };
            if self.script_scope
                && binding
                    .as_ref()
                    .is_some_and(|binding| binding.scope == self.root_scope)
            {
                let binding = binding.as_ref().expect("checked above");
                if let Some(local) = self.local_by_id(binding.id).cloned() {
                    return self.initialize_local(&local, register, declarator.span);
                }
                let lexical = self
                    .environments
                    .global_lexical(binding.id)
                    .ok_or(CompileError::BindingOverflow)?;
                let scope_name = self.global_lexical_binding(lexical)?;
                self.emit(
                    Opcode::InitializeGlobalLexical,
                    &[register.index(), scope_name],
                    declarator.span,
                )?;
                continue;
            }
            self.bind_pattern(
                &declarator.pattern,
                register,
                declaration.kind == HirVariableDeclarationKind::Let,
            )?;
        }
        Ok(())
    }

    /// Initializes one TDZ-backed environment binding without confusing it with assignment.
    fn initialize_local(
        &mut self,
        binding: &LocalBinding,
        value: RegisterId,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        match binding.storage {
            LocalStorage::Environment { slot, .. } => {
                let depth = self.environment_access_depth(binding)?;
                self.emit(
                    Opcode::InitializeEnvironment,
                    &[value.index(), depth, slot],
                    span,
                )
            }
            LocalStorage::Register(register) => {
                self.emit(Opcode::Move, &[register.index(), value.index()], span)
            }
        }
    }

    /// Executes var initializers at their source position against bindings instantiated at entry.
    pub(super) fn var_initializers(
        &mut self,
        declaration: &HirVariableDeclaration,
    ) -> Result<(), CompileError> {
        for declarator in declaration.declarators.iter() {
            let Some(initializer) = declarator.initializer.as_ref() else {
                continue;
            };
            let value = self.expression(initializer)?;
            self.initialize_declared_pattern(&declarator.pattern, value)?;
        }
        Ok(())
    }

    /// Destructures into declaration bindings that were instantiated before this source position.
    pub(super) fn initialize_declared_pattern(
        &mut self,
        pattern: &HirPattern,
        value: RegisterId,
    ) -> Result<(), CompileError> {
        match &pattern.kind {
            HirPatternKind::Binding(binding) => {
                self.write_var_binding(binding, value, pattern.span)
            }
            HirPatternKind::Default {
                target,
                initializer,
            } => {
                let value =
                    self.default_pattern_value(value, initializer, target.inferred_name())?;
                self.initialize_declared_pattern(target, value)
            }
            HirPatternKind::Object { properties, rest } => {
                self.require_object_coercible(value, pattern.span)?;
                let exclusions = rest
                    .as_ref()
                    .map(|_| self.create_exclusion_list(properties.len(), pattern.span))
                    .transpose()?;
                for property in properties.iter() {
                    let (property_value, key) =
                        self.pattern_property_with_key(value, &property.key, property.span)?;
                    if let Some(exclusions) = exclusions {
                        self.exclude_pattern_key(exclusions, key, property.span)?;
                    }
                    self.initialize_declared_pattern(&property.target, property_value)?;
                }
                if let Some(rest) = rest {
                    let target = self.register()?;
                    self.emit(Opcode::CreateObject, &[target.index()], pattern.span)?;
                    self.emit(
                        Opcode::CopyDataProperties,
                        &[
                            target.index(),
                            value.index(),
                            exclusions.expect("rest allocates exclusions").index(),
                        ],
                        pattern.span,
                    )?;
                    self.initialize_declared_pattern(rest, target)?;
                }
                Ok(())
            }
            HirPatternKind::Array { elements, rest } => {
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
                    if let Some(element) = element {
                        let item = self.pattern_property(
                            next,
                            &HirObjectPropertyKey::Static("value".into()),
                            pattern.span,
                        )?;
                        self.initialize_declared_pattern(element, item)?;
                    }
                    self.emit_jump(end, pattern.span)?;
                    self.builder
                        .bind_label(use_undefined)
                        .map_err(CompileError::Builder)?;
                    if let Some(element) = element {
                        let undefined = self.load_undefined(pattern.span)?;
                        self.initialize_declared_pattern(element, undefined)?;
                    }
                    self.builder
                        .bind_label(end)
                        .map_err(CompileError::Builder)?;
                }
                if let Some(rest) = rest {
                    let array = self.collect_iterator_rest(iterator, pattern.span)?;
                    self.initialize_declared_pattern(rest, array)?;
                }
                self.close_iterator_normally(iterator, pattern.span)
            }
            HirPatternKind::Assignment(_) => {
                Err(self.unsupported(pattern.span, "destructuring pattern bytecode"))
            }
        }
    }

    /// Stores one already-instantiated var leaf without redeclaring its environment slot.
    fn write_var_binding(
        &mut self,
        binding: &crate::HirBinding,
        value: RegisterId,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        if let Some(local) = self.local_by_id(binding.id).cloned() {
            return self.write_local(&local, value, span);
        }
        if self.script_scope {
            let scope_name = self.global_binding(&binding.name, true)?;
            return self.emit(Opcode::StoreScope, &[value.index(), scope_name], span);
        }
        Err(self.unsupported(span, "uninstantiated var binding"))
    }

    #[inline(always)]
    pub(super) fn local_by_id(&self, id: BindingId) -> Option<&LocalBinding> {
        self.locals.iter().rev().find(|binding| binding.id == id)
    }

    #[inline(always)]
    pub(super) fn local_reference(
        &self,
        reference: &HirIdentifierReference,
    ) -> Option<&LocalBinding> {
        reference.binding.and_then(|id| self.local_by_id(id))
    }

    pub(super) fn require_global_reference(
        &self,
        reference: &HirIdentifierReference,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        if reference.binding.is_some() && reference.binding_scope != Some(self.root_scope) {
            return Err(self.unsupported(span, "captured binding requires environment storage"));
        }
        Ok(())
    }

    pub(super) fn load_immediate(
        &mut self,
        value: u32,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let register = self.register()?;
        self.emit(Opcode::LoadImmediate, &[register.index(), value], span)?;
        Ok(register)
    }

    pub(super) fn load_undefined(&mut self, span: SourceSpan) -> Result<RegisterId, CompileError> {
        let register = self.register()?;
        self.emit(Opcode::LoadUndefined, &[register.index()], span)?;
        Ok(register)
    }

    pub(super) fn load_null(&mut self, span: SourceSpan) -> Result<RegisterId, CompileError> {
        let register = self.register()?;
        self.emit(Opcode::LoadNull, &[register.index()], span)?;
        Ok(register)
    }

    pub(super) fn load_boolean(
        &mut self,
        value: bool,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let register = self.register()?;
        let opcode = if value {
            Opcode::LoadTrue
        } else {
            Opcode::LoadFalse
        };
        self.emit(opcode, &[register.index()], span)?;
        Ok(register)
    }

    pub(super) fn register(&mut self) -> Result<RegisterId, CompileError> {
        let register = RegisterId::new(self.next_register);
        self.next_register = self
            .next_register
            .checked_add(1)
            .ok_or(CompileError::RegisterOverflow)?;
        Ok(register)
    }

    pub(super) fn add_binding_plan(
        &mut self,
        entry: BindingPlanEntry,
    ) -> Result<u32, CompileError> {
        if let Some(index) = self
            .binding_plan
            .iter()
            .position(|existing| existing == &entry)
        {
            return u32::try_from(index).map_err(|_| CompileError::BindingOverflow);
        }
        let index =
            u32::try_from(self.binding_plan.len()).map_err(|_| CompileError::BindingOverflow)?;
        self.binding_plan.push(entry);
        Ok(index)
    }

    /// Materializes one ancestor capture as this function's immutable binding-plan entry.
    pub(super) fn captured_reference(
        &mut self,
        reference: &HirIdentifierReference,
    ) -> Result<Option<LocalBinding>, CompileError> {
        let Some(binding) = reference.binding else {
            return Ok(None);
        };
        let Some((depth, slot, class_environment)) =
            self.environments.reference_slot(self.active_scope, binding)
        else {
            return Ok(None);
        };
        self.add_binding_plan(BindingPlanEntry {
            name: slot.name.clone(),
            location: if class_environment {
                BindingLocation::ClassEnvironment {
                    depth,
                    slot: slot.slot,
                }
            } else {
                BindingLocation::Environment {
                    depth,
                    slot: slot.slot,
                }
            },
            mutable: slot.mutable,
        })?;
        Ok(Some(LocalBinding {
            id: binding,
            storage: LocalStorage::Environment {
                depth,
                slot: slot.slot,
            },
            mutable: slot.mutable,
        }))
    }

    /// Resolves one lexical private name and publishes verifier-visible class-slot metadata.
    pub(super) fn private_reference(
        &mut self,
        private_name: &crate::hir::HirPrivateName,
    ) -> Result<(u32, u32), CompileError> {
        let (depth, slot, name) = {
            let (depth, slot) = self
                .environments
                .private_reference_slot(self.active_scope, private_name.id)
                .ok_or_else(|| self.unsupported(SourceSpan { start: 0, end: 0 }, "private name"))?;
            (depth, slot.slot, slot.name.clone())
        };
        self.add_binding_plan(BindingPlanEntry {
            name,
            location: BindingLocation::ClassEnvironment { depth, slot },
            mutable: false,
        })?;
        Ok((depth, slot))
    }

    pub(super) fn read_local(
        &mut self,
        binding: &LocalBinding,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        match binding.storage {
            LocalStorage::Register(register) => Ok(register),
            LocalStorage::Environment { slot, .. } => {
                let depth = self.environment_access_depth(binding)?;
                let destination = self.register()?;
                self.emit(
                    Opcode::LoadEnvironment,
                    &[destination.index(), depth, slot],
                    span,
                )?;
                Ok(destination)
            }
        }
    }

    pub(super) fn snapshot_local(
        &mut self,
        binding: &LocalBinding,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let value = self.read_local(binding, span)?;
        if matches!(binding.storage, LocalStorage::Environment { .. }) {
            return Ok(value);
        }
        let snapshot = self.register()?;
        self.emit(Opcode::Move, &[snapshot.index(), value.index()], span)?;
        Ok(snapshot)
    }

    pub(super) fn write_local(
        &mut self,
        binding: &LocalBinding,
        value: RegisterId,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        if !self.strict && self.environments.is_function_self_binding(binding.id) {
            // Sloppy assignment to a named-function expression's immutable internal name is a
            // no-op even when the reference crosses one or more nested arrow environments.
            return Ok(());
        }
        match binding.storage {
            LocalStorage::Register(register) => {
                self.emit(Opcode::Move, &[register.index(), value.index()], span)
            }
            LocalStorage::Environment { slot, .. } => self.emit(
                Opcode::StoreEnvironment,
                &[value.index(), self.environment_access_depth(binding)?, slot],
                span,
            ),
        }
    }

    /// Resolves a stable binding identity against the lexical environment active at this use.
    fn environment_access_depth(&self, binding: &LocalBinding) -> Result<u32, CompileError> {
        let LocalStorage::Environment { slot, .. } = binding.storage else {
            return Err(CompileError::BindingOverflow);
        };
        let (depth, resolved_slot, _) = self
            .environments
            .reference_slot(self.active_scope, binding.id)
            .ok_or(CompileError::BindingOverflow)?;
        if resolved_slot.slot != slot {
            return Err(CompileError::BindingOverflow);
        }
        Ok(depth)
    }

    /// Publishes one local binding and initializes promoted parameters through verified bytecode.
    pub(super) fn add_local(
        &mut self,
        binding: &crate::HirBinding,
        register: Option<RegisterId>,
        mutable: bool,
    ) -> Result<(), CompileError> {
        let storage = if let Some(function_scope) = self.function_scope
            && let Some(slot) = self.environments.local_slot(function_scope, binding.id)
        {
            self.add_binding_plan(BindingPlanEntry {
                name: slot.name.clone(),
                location: BindingLocation::Environment {
                    depth: 0,
                    slot: slot.slot,
                },
                mutable: slot.mutable,
            })?;
            if let Some(register) = register {
                let opcode = if slot.initialized {
                    Opcode::StoreEnvironment
                } else {
                    Opcode::InitializeEnvironment
                };
                self.emit(opcode, &[register.index(), 0, slot.slot], binding.span)?;
            }
            LocalStorage::Environment {
                depth: 0,
                slot: slot.slot,
            }
        } else {
            let register = register.ok_or(CompileError::RegisterOverflow)?;
            self.add_binding_plan(BindingPlanEntry {
                name: binding.name.clone(),
                location: BindingLocation::FrameRegister(register),
                mutable,
            })?;
            LocalStorage::Register(register)
        };
        self.locals.push(LocalBinding {
            id: binding.id,
            storage,
            mutable,
        });
        Ok(())
    }

    /// Publishes one entry-owned environment binding before script statements execute.
    pub(super) fn add_environment_slot(
        &mut self,
        slot: &super::CapturedSlot,
    ) -> Result<(), CompileError> {
        self.add_binding_plan(BindingPlanEntry {
            name: slot.name.clone(),
            location: if self.module_scope {
                BindingLocation::ModuleCell { slot: slot.slot }
            } else {
                BindingLocation::Environment {
                    depth: 0,
                    slot: slot.slot,
                }
            },
            mutable: slot.mutable,
        })?;
        self.locals.push(LocalBinding {
            id: slot.id,
            storage: LocalStorage::Environment {
                depth: 0,
                slot: slot.slot,
            },
            mutable: slot.mutable,
        });
        Ok(())
    }

    /// Records one global-property binding once while returning its shared module name index.
    pub(super) fn global_binding(
        &mut self,
        name: &std::sync::Arc<str>,
        mutable: bool,
    ) -> Result<u32, CompileError> {
        let scope_name = self.scope_name(name)?;
        let entry = BindingPlanEntry {
            name: name.clone(),
            location: BindingLocation::GlobalProperty,
            mutable,
        };
        if !self.binding_plan.contains(&entry) {
            self.binding_plan.push(entry);
        }
        Ok(scope_name)
    }

    pub(super) fn global_lexical_binding(
        &mut self,
        lexical: &GlobalLexicalPlan,
    ) -> Result<u32, CompileError> {
        let scope_name = self.scope_name(&lexical.name)?;
        self.add_binding_plan(BindingPlanEntry {
            name: lexical.name.clone(),
            location: BindingLocation::GlobalLexical,
            mutable: lexical.mutable,
        })?;
        Ok(scope_name)
    }

    pub(super) fn resolved_global_binding(
        &mut self,
        reference: &HirIdentifierReference,
        mutable: bool,
    ) -> Result<u32, CompileError> {
        if let Some(lexical) = reference
            .binding
            .and_then(|binding| self.environments.global_lexical(binding))
        {
            return self.global_lexical_binding(lexical);
        }
        self.global_binding(&reference.name, mutable)
    }

    /// Returns a module-stable scope-name index while retaining only one owned copy per spelling.
    pub(super) fn scope_name(&mut self, name: &std::sync::Arc<str>) -> Result<u32, CompileError> {
        if let Some(index) = self
            .scope_names
            .iter()
            .position(|existing| existing.as_ref() == name.as_ref())
        {
            return u32::try_from(index).map_err(|_| CompileError::BindingOverflow);
        }
        let index =
            u32::try_from(self.scope_names.len()).map_err(|_| CompileError::BindingOverflow)?;
        self.scope_names.push(name.clone());
        Ok(index)
    }

    pub(super) fn emit_jump(
        &mut self,
        target: Label,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        self.builder
            .emit_jump(
                target,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map(|_| ())
            .map_err(CompileError::Builder)
    }

    /// Emits a direct branch or an explicit completion transfer when leaving a finalizer scope.
    pub(super) fn emit_control_jump(
        &mut self,
        target: ControlTarget,
        opcode: Opcode,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        if target.finally_depth == self.finally_depth {
            return self.emit_jump(target.label, span);
        }
        debug_assert!(target.finally_depth < self.finally_depth);
        self.builder
            .emit_abrupt_jump(
                opcode,
                target.label,
                BytecodeSourceSpan {
                    start: span.start,
                    end: span.end,
                },
            )
            .map(|_| ())
            .map_err(CompileError::Builder)
    }

    pub(super) fn current_break_target(
        &self,
        name: Option<&str>,
        span: SourceSpan,
    ) -> Result<ControlTarget, CompileError> {
        self.break_targets
            .iter()
            .rev()
            .find(|target| target.name.as_deref() == name)
            .cloned()
            .ok_or_else(|| CompileError::UnsupportedSyntax {
                source_name: self.source_name.clone(),
                span,
                syntax: "break outside breakable statement",
            })
    }

    pub(super) fn current_continue_target(
        &self,
        name: Option<&str>,
        span: SourceSpan,
    ) -> Result<ControlTarget, CompileError> {
        self.continue_targets
            .iter()
            .rev()
            .find(|target| target.name.as_deref() == name)
            .cloned()
            .ok_or_else(|| self.unsupported(span, "continue outside loop"))
    }

    #[inline]
    pub(super) fn control_target(&self, label: Label) -> ControlTarget {
        ControlTarget {
            label,
            finally_depth: self.finally_depth,
            name: None,
        }
    }

    #[inline]
    pub(super) fn named_control_target(
        &self,
        label: Label,
        name: std::sync::Arc<str>,
    ) -> ControlTarget {
        ControlTarget {
            label,
            finally_depth: self.finally_depth,
            name: Some(name),
        }
    }

    #[inline]
    pub(super) fn control_target_at_depth(
        &self,
        label: Label,
        finally_depth: u32,
    ) -> ControlTarget {
        ControlTarget {
            label,
            finally_depth,
            name: None,
        }
    }

    /// Publishes one unnamed loop target plus aliases for directly enclosing labels.
    pub(super) fn push_loop_control_targets(
        &mut self,
        break_target: ControlTarget,
        continue_label: Label,
    ) -> (usize, usize) {
        let break_checkpoint = self.break_targets.len();
        let continue_checkpoint = self.continue_targets.len();
        self.break_targets.push(break_target);
        self.continue_targets
            .push(self.control_target(continue_label));
        for name in core::mem::take(&mut self.pending_loop_labels) {
            let target = self.named_control_target(continue_label, name);
            self.continue_targets.push(target);
        }
        (break_checkpoint, continue_checkpoint)
    }

    #[inline]
    pub(super) fn pop_loop_control_targets(&mut self, checkpoints: (usize, usize)) {
        self.break_targets.truncate(checkpoints.0);
        self.continue_targets.truncate(checkpoints.1);
    }
}
