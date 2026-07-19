mod expression;
mod statement;

use tachyon_bytecode::{
    BindingLocation, BindingPlanEntry, BytecodeBuilder, BytecodeConstant, HandlerEntry,
    HandlerKind, Label, Opcode, RegisterId, SourceSpan as BytecodeSourceSpan,
};

use crate::hir::{HirAssignmentOperator, HirAssignmentTarget, HirForInLeft};
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

impl Lowerer<'_> {
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
            if self.script_scope && declarator.binding.scope == self.root_scope {
                let lexical = self
                    .environments
                    .global_lexical(declarator.binding.id)
                    .ok_or(CompileError::BindingOverflow)?;
                let scope_name = self.global_lexical_binding(lexical)?;
                self.emit(
                    Opcode::InitializeGlobalLexical,
                    &[register.index(), scope_name],
                    declarator.span,
                )?;
                continue;
            }
            self.add_local(
                &declarator.binding,
                Some(register),
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
            if let Some(binding) = self.local_by_id(declarator.binding.id).cloned() {
                self.write_local(&binding, value, declarator.span)?;
            } else if self.script_scope {
                let scope_name = self.global_binding(&declarator.binding.name, true)?;
                self.emit(
                    Opcode::StoreScope,
                    &[value.index(), scope_name],
                    declarator.span,
                )?;
            } else {
                return Err(self.unsupported(declarator.span, "uninstantiated var binding"));
            }
        }
        Ok(())
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
