mod control;
mod instructions;
mod names_literals;
mod suspend_points;

use tachyon_bytecode::MAX_ENCODED_INSTRUCTION_WORDS;

use crate::{
    CompileError, HirCatchClause, HirFunction, HirPattern, HirPatternKind, HirProgram, HirStatement,
};

pub(super) struct ModuleCapacity {
    pub(super) constants: usize,
    pub(super) scope_names: usize,
}

pub(super) struct LoweringCapacity {
    pub(super) bytecode_words: usize,
    pub(super) labels: usize,
    pub(super) local_bindings: usize,
    pub(super) binding_plan: usize,
    pub(super) break_targets: usize,
    pub(super) continue_targets: usize,
    pub(super) loop_labels: usize,
    pub(super) handlers: usize,
    pub(super) suspend_points: usize,
    pub(super) max_handler_depth: u32,
    pub(super) max_completion_depth: u32,
}

/// Estimates both module-owned pools before lowering allocates either collection.
pub(super) fn estimate_module(hir: &HirProgram) -> Result<ModuleCapacity, CompileError> {
    Ok(ModuleCapacity {
        constants: names_literals::hir_literal_count(hir)?,
        scope_names: names_literals::hir_scope_name_capacity(hir)?,
    })
}

/// Estimates storage for the actual var-binding collector owned by the lowering module.
pub(super) fn estimate_var_bindings(statements: &[HirStatement]) -> Result<usize, CompileError> {
    control::statements_binding_count(statements)
}

/// Estimates every entry-function collection from the owned HIR before bytecode emission.
pub(super) fn estimate_entry(
    hir: &HirProgram,
    var_binding_count: usize,
    global_lexical_count: usize,
    has_control_flow: bool,
    has_expression: bool,
) -> Result<LoweringCapacity, CompileError> {
    let function_declaration_count = hir
        .statements()
        .iter()
        .filter(|statement| {
            matches!(
                statement.kind,
                crate::HirStatementKind::FunctionDeclaration(_)
            )
        })
        .count();
    let result_instruction_count = if has_control_flow {
        control::statements_expression_count(hir.statements())?
            .checked_add(2)
            .ok_or(CompileError::LoweringCapacityOverflow {
                collection: "entry completion instructions",
            })?
    } else if has_expression {
        1
    } else {
        2
    };
    let instruction_upper_bound = instructions::hir_instruction_count(hir)?
        .checked_add(var_binding_count)
        .and_then(|count| count.checked_add(global_lexical_count))
        .and_then(|count| count.checked_add(function_declaration_count))
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "global var instantiation instructions",
        })?
        .checked_add(result_instruction_count)
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "bytecode instructions",
        })?;
    let bytecode_words = instruction_upper_bound
        .checked_mul(MAX_ENCODED_INSTRUCTION_WORDS)
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "bytecode words",
        })?;
    let handlers = control::statements_handler_count(hir.statements())?;
    let max_handler_depth = control::statements_handler_depth(hir.statements())?;
    let max_completion_depth = control::statements_finally_depth(hir.statements())?;
    let binding_plan = control::hir_binding_count(hir)?
        .checked_add(names_literals::statements_scope_name_count(
            hir.statements(),
        )?)
        .and_then(|count| count.checked_add(var_binding_count))
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "entry binding plan",
        })?;
    let labels = control::hir_label_count(hir)?;
    let local_bindings = control::hir_binding_count(hir)?;
    let loop_count = control::statements_loop_count(hir.statements())?;
    let label_count = control::statements_labeled_count(hir.statements())?;
    let break_targets = control::statements_switch_count(hir.statements())?
        .checked_add(label_count)
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "break targets",
        })?;
    let continue_targets =
        loop_count
            .checked_add(label_count)
            .ok_or(CompileError::LoweringCapacityOverflow {
                collection: "continue targets",
            })?;

    Ok(LoweringCapacity {
        bytecode_words,
        labels,
        local_bindings,
        binding_plan,
        break_targets,
        continue_targets,
        loop_labels: label_count,
        handlers,
        suspend_points: 0,
        max_handler_depth,
        max_completion_depth,
    })
}

/// Estimates every ordinary-function collection from its owned stencil before emission.
pub(super) fn estimate_function(
    function: &HirFunction,
    var_initialization_count: usize,
) -> Result<LoweringCapacity, CompileError> {
    let parameter_binding_count = function
        .parameters
        .iter()
        .try_fold(0_usize, |count, parameter| {
            checked_pattern_binding_count_add(count, parameter)
        })?;
    let rest_binding_count = function
        .rest_parameter
        .as_ref()
        .map(pattern_binding_count)
        .transpose()?
        .unwrap_or(0);
    let parameter_initializer_count = function
        .parameter_initializers
        .iter()
        .filter(|initializer| initializer.is_some())
        .count();
    let parameter_initializer_instructions =
        instructions::parameter_initializer_instruction_count(&function.parameter_initializers)?;
    let bytecode_words = instructions::statements_instruction_count(&function.body)?
        .checked_add(var_initialization_count)
        .and_then(|count| count.checked_add(parameter_binding_count))
        .and_then(|count| count.checked_add(usize::from(function.rest_parameter.is_some())))
        .and_then(|count| count.checked_add(rest_binding_count))
        .and_then(|count| count.checked_add(parameter_initializer_instructions))
        .and_then(|count| {
            count.checked_add(
                if function.kind == crate::HirFunctionKind::DefaultDerivedConstructor {
                    3 + usize::from(function.initialize_instance_elements)
                } else {
                    2 + usize::from(
                        function.initialize_instance_elements
                            && matches!(
                                function.kind,
                                crate::HirFunctionKind::BaseClassConstructor
                                    | crate::HirFunctionKind::DefaultBaseConstructor
                            ),
                    )
                },
            )
        })
        .and_then(|count| count.checked_mul(MAX_ENCODED_INSTRUCTION_WORDS))
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "function bytecode words",
        })?;
    let handlers = control::statements_handler_count(&function.body)?;
    let max_handler_depth = control::statements_handler_depth(&function.body)?;
    let max_completion_depth = control::statements_finally_depth(&function.body)?;
    let statement_bindings = control::statements_binding_count(&function.body)?;
    let local_bindings = function
        .parameters
        .iter()
        .try_fold(0_usize, |count, parameter| {
            checked_pattern_binding_count_add(count, parameter)
        })?
        .checked_add(rest_binding_count)
        .and_then(|count| count.checked_add(usize::from(function.self_binding.is_some())))
        .and_then(|count| count.checked_add(statement_bindings))
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "function local bindings",
        })?;
    let parameter_scope_names =
        names_literals::parameter_scope_name_count(&function.parameter_initializers)?;
    let binding_plan = local_bindings
        .checked_add(names_literals::statements_scope_name_count(&function.body)?)
        .and_then(|count| count.checked_add(parameter_scope_names))
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "function binding plan",
        })?;
    let labels = control::statements_label_count(&function.body)?
        .checked_add(parameter_initializer_count)
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "bytecode labels",
        })?;
    let loop_count = control::statements_loop_count(&function.body)?;
    let label_count = control::statements_labeled_count(&function.body)?;
    let break_targets = control::statements_switch_count(&function.body)?
        .checked_add(label_count)
        .ok_or(CompileError::LoweringCapacityOverflow {
            collection: "break targets",
        })?;
    let continue_targets =
        loop_count
            .checked_add(label_count)
            .ok_or(CompileError::LoweringCapacityOverflow {
                collection: "continue targets",
            })?;
    let suspend_points = suspend_points::function_suspend_point_count(
        &function.body,
        &function.parameter_initializers,
    )?;

    Ok(LoweringCapacity {
        bytecode_words,
        labels,
        local_bindings,
        binding_plan,
        break_targets,
        continue_targets,
        loop_labels: label_count,
        handlers,
        suspend_points,
        max_handler_depth,
        max_completion_depth,
    })
}

/// Counts every declaration leaf in a parameter pattern for exact lowering storage estimates.
fn pattern_binding_count(pattern: &HirPattern) -> Result<usize, CompileError> {
    match &pattern.kind {
        HirPatternKind::Binding(_) => Ok(1),
        HirPatternKind::Assignment(_) => Ok(0),
        HirPatternKind::Default { target, .. } => pattern_binding_count(target),
        HirPatternKind::Array { elements, rest } => {
            let mut count = 0;
            for element in elements.iter().flatten() {
                count = checked_pattern_binding_count_add(count, element)?;
            }
            if let Some(rest) = rest {
                count = checked_pattern_binding_count_add(count, rest)?;
            }
            Ok(count)
        }
        HirPatternKind::Object { properties, rest } => {
            let mut count = 0;
            for property in properties.iter() {
                count = checked_pattern_binding_count_add(count, &property.target)?;
            }
            if let Some(rest) = rest {
                count = checked_pattern_binding_count_add(count, rest)?;
            }
            Ok(count)
        }
    }
}

fn checked_pattern_binding_count_add(
    count: usize,
    pattern: &HirPattern,
) -> Result<usize, CompileError> {
    count.checked_add(pattern_binding_count(pattern)?).ok_or(
        CompileError::LoweringCapacityOverflow {
            collection: "function parameter bindings",
        },
    )
}

/// Adds every owned try child with the same checked collection-specific counter.
fn try_children_count(
    block: &[HirStatement],
    handler: Option<&HirCatchClause>,
    finalizer: Option<&[HirStatement]>,
    collection: &'static str,
    counter: fn(&[HirStatement]) -> Result<usize, CompileError>,
) -> Result<usize, CompileError> {
    let mut count = counter(block)?;
    if let Some(handler) = handler {
        count = checked_count_add(count, counter(&handler.body)?, collection)?;
    }
    if let Some(finalizer) = finalizer {
        count = checked_count_add(count, counter(finalizer)?, collection)?;
    }
    Ok(count)
}

fn checked_count_add(
    total: usize,
    next: usize,
    collection: &'static str,
) -> Result<usize, CompileError> {
    total
        .checked_add(next)
        .ok_or(CompileError::LoweringCapacityOverflow { collection })
}
