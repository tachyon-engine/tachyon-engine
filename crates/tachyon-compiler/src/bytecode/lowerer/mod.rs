mod expression;
mod statement;

use tachyon_bytecode::{
    BindingLocation, BindingPlanEntry, BytecodeBuilder, BytecodeConstant, HandlerEntry,
    HandlerKind, Label, Opcode, RegisterId, SourceSpan as BytecodeSourceSpan,
};

use crate::hir::{
    HirAssignmentOperator, HirAssignmentTarget, HirForInLeft, HirPattern, HirPatternKind,
};
use crate::{
    BindingId, CompileError, HirBinaryOperator, HirCatchClause, HirExpression, HirExpressionKind,
    HirForInitializer, HirFunctionDeclaration, HirIdentifierReference, HirLogicalOperator,
    HirObjectPropertyKey, HirStatement, HirStatementKind, HirSwitchCase, HirUnaryOperator,
    HirUpdateOperator, HirVariableDeclaration, HirVariableDeclarationKind, ScopeId, SourceName,
    SourceSpan,
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
    pub(super) handlers: Vec<Option<HandlerEntry>>,
    pub(super) finally_depth: u32,
    pub(super) next_register: u32,
    pub(super) source_name: SourceName,
    pub(super) script_scope: bool,
    pub(super) root_scope: ScopeId,
    pub(super) function_scope: Option<ScopeId>,
    pub(super) environments: &'a EnvironmentPlans,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ControlTarget {
    label: Label,
    finally_depth: u32,
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
    /// Destructures one object binding into locals, preserving property and default evaluation order.
    pub(super) fn bind_pattern(
        &mut self,
        pattern: &HirPattern,
        value: RegisterId,
        mutable: bool,
    ) -> Result<(), CompileError> {
        match &pattern.kind {
            HirPatternKind::Binding(binding) => self.add_local(binding, Some(value), mutable),
            HirPatternKind::Default {
                target,
                initializer,
            } => {
                let value =
                    self.default_pattern_value(value, initializer, target.inferred_name())?;
                self.bind_pattern(target, value, mutable)
            }
            HirPatternKind::Object { properties, rest } => {
                if rest.is_some() {
                    return Err(self.unsupported(pattern.span, "object rest bytecode"));
                }
                self.require_object_coercible(value, pattern.span)?;
                for property in properties.iter() {
                    let property_value =
                        self.pattern_property(value, &property.key, property.span)?;
                    self.bind_pattern(&property.target, property_value, mutable)?;
                }
                Ok(())
            }
            HirPatternKind::Array { elements, rest } => {
                if rest.is_some() {
                    return Err(self.unsupported(pattern.span, "array rest bytecode"));
                }
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
        let symbol = self.register()?;
        let symbol_scope = self.global_binding(&std::sync::Arc::from("Symbol"), false)?;
        self.emit(Opcode::LoadScope, &[symbol.index(), symbol_scope], span)?;
        let iterator_key = self.register()?;
        let iterator_atom = self.scope_name(&std::sync::Arc::from("iterator"))?;
        self.emit(
            Opcode::GetById,
            &[iterator_key.index(), symbol.index(), iterator_atom],
            span,
        )?;
        self.prepare_property_key(iterator_key, object, false, span)?;
        let iterator = self.computed_method_call(object, iterator_key, span)?;
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

    /// Calls cached `next`, then updates the record's done register from its result object.
    pub(super) fn iterator_next(
        &mut self,
        iterator: IteratorRegisters,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let result = self.call_receiver(iterator.receiver, iterator.next, span)?;
        let done =
            self.pattern_property(result, &HirObjectPropertyKey::Static("done".into()), span)?;
        self.emit(Opcode::Move, &[iterator.done.index(), done.index()], span)?;
        Ok(result)
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
        let destination = self.register()?;
        match key {
            HirObjectPropertyKey::Static(property) => {
                let property = self.scope_name(property)?;
                self.emit(
                    Opcode::GetById,
                    &[destination.index(), object.index(), property],
                    span,
                )?;
            }
            HirObjectPropertyKey::Computed(expression) => {
                let property = self.expression(expression)?;
                self.emit(
                    Opcode::GetByValue,
                    &[destination.index(), object.index(), property.index()],
                    span,
                )?;
            }
        }
        Ok(destination)
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
        if matches!(initializer.kind, HirExpressionKind::Function(_))
            && let Some(name) = inferred_name
        {
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
        self.emit(Opcode::CreateClosure, &[register.index(), function], span)?;
        let scope_name = self.global_binding(&declaration.binding.name, true)?;
        self.emit(Opcode::StoreScope, &[register.index(), scope_name], span)?;
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
    /// Extracts a simple pattern leaf or reports the unimplemented destructuring backend.
    #[inline(always)]
    pub(super) fn simple_binding<'a>(
        &self,
        pattern: &'a crate::HirPattern,
    ) -> Result<&'a crate::HirBinding, CompileError> {
        pattern
            .binding()
            .ok_or_else(|| self.unsupported(pattern.span, "destructuring pattern bytecode"))
    }

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
            self.initialize_var_pattern(&declarator.pattern, value)?;
        }
        Ok(())
    }

    /// Destructures one var initializer into bindings instantiated at function or script entry.
    fn initialize_var_pattern(
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
                self.initialize_var_pattern(target, value)
            }
            HirPatternKind::Object { properties, rest } => {
                if rest.is_some() {
                    return Err(self.unsupported(pattern.span, "object rest bytecode"));
                }
                self.require_object_coercible(value, pattern.span)?;
                for property in properties.iter() {
                    let property_value =
                        self.pattern_property(value, &property.key, property.span)?;
                    self.initialize_var_pattern(&property.target, property_value)?;
                }
                Ok(())
            }
            HirPatternKind::Array { elements, rest } => {
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
                    if let Some(element) = element {
                        let item = self.pattern_property(
                            next,
                            &HirObjectPropertyKey::Static("value".into()),
                            pattern.span,
                        )?;
                        self.initialize_var_pattern(element, item)?;
                    }
                    self.emit_jump(end, pattern.span)?;
                    self.builder
                        .bind_label(use_undefined)
                        .map_err(CompileError::Builder)?;
                    if let Some(element) = element {
                        let undefined = self.load_undefined(pattern.span)?;
                        self.initialize_var_pattern(element, undefined)?;
                    }
                    self.builder
                        .bind_label(end)
                        .map_err(CompileError::Builder)?;
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
        let (Some(function_scope), Some(binding)) = (self.function_scope, reference.binding) else {
            return Ok(None);
        };
        let Some((depth, slot)) = self.environments.reference_slot(function_scope, binding) else {
            return Ok(None);
        };
        self.add_binding_plan(BindingPlanEntry {
            name: slot.name.clone(),
            location: BindingLocation::Environment {
                depth,
                slot: slot.slot,
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

    pub(super) fn read_local(
        &mut self,
        binding: &LocalBinding,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        match binding.storage {
            LocalStorage::Register(register) => Ok(register),
            LocalStorage::Environment { depth, slot } => {
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
        match binding.storage {
            LocalStorage::Register(register) => {
                self.emit(Opcode::Move, &[register.index(), value.index()], span)
            }
            LocalStorage::Environment { depth, slot } => self.emit(
                Opcode::StoreEnvironment,
                &[value.index(), depth, slot],
                span,
            ),
        }
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
                self.emit(
                    Opcode::StoreEnvironment,
                    &[register.index(), 0, slot.slot],
                    binding.span,
                )?;
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
        span: SourceSpan,
    ) -> Result<ControlTarget, CompileError> {
        self.break_targets
            .last()
            .copied()
            .ok_or_else(|| CompileError::UnsupportedSyntax {
                source_name: self.source_name.clone(),
                span,
                syntax: "break outside breakable statement",
            })
    }

    pub(super) fn current_continue_target(
        &self,
        span: SourceSpan,
    ) -> Result<ControlTarget, CompileError> {
        self.continue_targets
            .last()
            .copied()
            .ok_or_else(|| self.unsupported(span, "continue outside loop"))
    }

    #[inline]
    pub(super) const fn control_target(&self, label: Label) -> ControlTarget {
        ControlTarget {
            label,
            finally_depth: self.finally_depth,
        }
    }
}
