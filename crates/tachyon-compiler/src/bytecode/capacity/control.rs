use super::{checked_count_add, try_children_count};
use crate::hir::{HirAssignmentOperator, HirAssignmentTarget, HirForInLeft};
use crate::{
    CompileError, HirExpression, HirExpressionKind, HirForInitializer, HirObjectPropertyKey,
    HirProgram, HirStatement, HirStatementKind, HirSwitchCase,
};

/// Counts handler records exactly, including nested ranges in every try arm.
pub(super) fn statements_handler_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let nested = match &statement.kind {
            HirStatementKind::Block(statements) => statements_handler_count(statements)?,
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                let mut nested = statements_handler_count(core::slice::from_ref(consequent))?;
                if let Some(alternate) = alternate {
                    nested = checked_count_add(
                        nested,
                        statements_handler_count(core::slice::from_ref(alternate))?,
                        "exception handlers",
                    )?;
                }
                nested
            }
            HirStatementKind::For { body, .. }
            | HirStatementKind::ForIn { body, .. }
            | HirStatementKind::Loop { body, .. } => {
                statements_handler_count(core::slice::from_ref(body))?
            }
            HirStatementKind::Switch { cases, .. } => {
                let mut nested = 0;
                for case in cases.iter() {
                    nested = checked_count_add(
                        nested,
                        statements_handler_count(&case.consequent)?,
                        "exception handlers",
                    )?;
                }
                nested
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                let nested = try_children_count(
                    block,
                    handler.as_ref(),
                    finalizer.as_deref(),
                    "exception handlers",
                    statements_handler_count,
                )?;
                checked_count_add(nested, usize::from(handler.is_some()), "exception handlers")?
            }
            HirStatementKind::Expression(_)
            | HirStatementKind::VariableDeclaration(_)
            | HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, nested, "exception handlers")?;
    }
    Ok(count)
}

/// Computes exact simultaneous catch-range depth rather than total handler count.
pub(super) fn statements_handler_depth(statements: &[HirStatement]) -> Result<u32, CompileError> {
    let mut depth = 0;
    for statement in statements {
        let nested = match &statement.kind {
            HirStatementKind::Block(statements) => statements_handler_depth(statements)?,
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                let consequent = statements_handler_depth(core::slice::from_ref(consequent))?;
                let alternate = alternate
                    .as_ref()
                    .map(|statement| statements_handler_depth(core::slice::from_ref(statement)))
                    .transpose()?
                    .unwrap_or(0);
                consequent.max(alternate)
            }
            HirStatementKind::For { body, .. }
            | HirStatementKind::ForIn { body, .. }
            | HirStatementKind::Loop { body, .. } => {
                statements_handler_depth(core::slice::from_ref(body))?
            }
            HirStatementKind::Switch { cases, .. } => {
                let mut nested = 0;
                for case in cases.iter() {
                    nested = nested.max(statements_handler_depth(&case.consequent)?);
                }
                nested
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                let block = statements_handler_depth(block)?
                    .checked_add(u32::from(handler.is_some()))
                    .ok_or(CompileError::LoweringCapacityOverflow {
                        collection: "exception handler depth",
                    })?;
                let handler = handler
                    .as_ref()
                    .map(|handler| statements_handler_depth(&handler.body))
                    .transpose()?
                    .unwrap_or(0);
                let finalizer = finalizer
                    .as_ref()
                    .map(|statements| statements_handler_depth(statements))
                    .transpose()?
                    .unwrap_or(0);
                block.max(handler).max(finalizer)
            }
            HirStatementKind::Expression(_)
            | HirStatementKind::VariableDeclaration(_)
            | HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => 0,
        };
        depth = depth.max(nested);
    }
    Ok(depth)
}

pub(super) fn hir_binding_count(hir: &HirProgram) -> Result<usize, CompileError> {
    statements_binding_count(hir.statements())
}

/// Counts all lexical bindings as a checked upper bound for the lowering-time local stack.
pub(super) fn statements_binding_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let statement_count = match &statement.kind {
            HirStatementKind::VariableDeclaration(declaration) => declaration.declarators.len(),
            HirStatementKind::FunctionDeclaration(_) => 1,
            HirStatementKind::Block(statements) => statements_binding_count(statements)?,
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                let mut nested = statements_binding_count(core::slice::from_ref(consequent))?;
                if let Some(alternate) = alternate {
                    nested = checked_count_add(
                        nested,
                        statements_binding_count(core::slice::from_ref(alternate))?,
                        "local bindings",
                    )?;
                }
                nested
            }
            HirStatementKind::For {
                initializer, body, ..
            } => {
                let initializer = match initializer {
                    Some(HirForInitializer::Variable(declaration)) => declaration.declarators.len(),
                    _ => 0,
                };
                checked_count_add(
                    initializer,
                    statements_binding_count(core::slice::from_ref(body))?,
                    "local bindings",
                )?
            }
            HirStatementKind::ForIn { left, body, .. } => {
                let head = usize::from(matches!(left, HirForInLeft::Variable(_)));
                checked_count_add(
                    head,
                    statements_binding_count(core::slice::from_ref(body))?,
                    "local bindings",
                )?
            }
            HirStatementKind::Loop { body, .. } => {
                statements_binding_count(core::slice::from_ref(body))?
            }
            HirStatementKind::Switch { cases, .. } => switch_binding_count(cases)?,
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                let nested = try_children_count(
                    block,
                    handler.as_ref(),
                    finalizer.as_deref(),
                    "local bindings",
                    statements_binding_count,
                )?;
                checked_count_add(
                    nested,
                    usize::from(
                        handler
                            .as_ref()
                            .is_some_and(|handler| handler.parameter.is_some()),
                    ),
                    "local bindings",
                )?
            }
            HirStatementKind::Expression(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "local bindings")?;
    }
    Ok(count)
}

/// Counts all case-block bindings as one conservative switch-scope capacity bound.
fn switch_binding_count(cases: &[HirSwitchCase]) -> Result<usize, CompileError> {
    let mut count = 0;
    for case in cases {
        count = checked_count_add(
            count,
            statements_binding_count(&case.consequent)?,
            "local bindings",
        )?;
    }
    Ok(count)
}

/// Counts every conditional label before bytecode construction so the builder's label vector stays fixed-size.
pub(super) fn hir_label_count(hir: &HirProgram) -> Result<usize, CompileError> {
    statements_label_count(hir.statements())
}

/// Counts structured-statement and expression labels before builder allocation.
pub(super) fn statements_label_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        match &statement.kind {
            HirStatementKind::Expression(expression) => {
                count = checked_count_add(
                    count,
                    expression_label_count(expression)?,
                    "bytecode labels",
                )?;
            }
            HirStatementKind::Loop { test, body, .. } => {
                count = checked_count_add(count, 3, "bytecode labels")?;
                count = checked_count_add(count, expression_label_count(test)?, "bytecode labels")?;
                count = checked_count_add(
                    count,
                    statements_label_count(core::slice::from_ref(body))?,
                    "bytecode labels",
                )?;
            }
            HirStatementKind::VariableDeclaration(declaration) => {
                for initializer in declaration
                    .declarators
                    .iter()
                    .filter_map(|declarator| declarator.initializer.as_ref())
                {
                    count = checked_count_add(
                        count,
                        expression_label_count(initializer)?,
                        "bytecode labels",
                    )?;
                }
            }
            HirStatementKind::Return(argument) => {
                if let Some(argument) = argument {
                    count = checked_count_add(
                        count,
                        expression_label_count(argument)?,
                        "bytecode labels",
                    )?;
                }
            }
            HirStatementKind::Throw(argument) => {
                count =
                    checked_count_add(count, expression_label_count(argument)?, "bytecode labels")?;
            }
            HirStatementKind::Block(statements) => {
                count = checked_count_add(
                    count,
                    statements_label_count(statements)?,
                    "bytecode labels",
                )?;
            }
            HirStatementKind::If {
                test,
                consequent,
                alternate,
            } => {
                count = checked_count_add(count, expression_label_count(test)?, "bytecode labels")?;
                count = checked_count_add(count, 2, "bytecode labels")?;
                count = checked_count_add(
                    count,
                    statements_label_count(core::slice::from_ref(consequent))?,
                    "bytecode labels",
                )?;
                if let Some(alternate) = alternate {
                    count = checked_count_add(
                        count,
                        statements_label_count(core::slice::from_ref(alternate))?,
                        "bytecode labels",
                    )?;
                }
            }
            HirStatementKind::For {
                initializer,
                test,
                update,
                body,
            } => {
                count = checked_count_add(
                    count,
                    for_label_count(initializer.as_ref(), test.as_ref(), update.as_ref(), body)?,
                    "bytecode labels",
                )?;
            }
            HirStatementKind::ForIn { left, right, body } => {
                let mut nested = expression_label_count(right)?;
                nested =
                    checked_count_add(nested, for_in_left_label_count(left)?, "bytecode labels")?;
                nested = checked_count_add(
                    nested,
                    statements_label_count(core::slice::from_ref(body))?,
                    "bytecode labels",
                )?;
                count = checked_count_add(count, nested, "bytecode labels")?;
                count = checked_count_add(count, 2, "bytecode labels")?;
            }
            HirStatementKind::Switch {
                discriminant,
                cases,
            } => {
                count = checked_count_add(
                    count,
                    switch_label_count(discriminant, cases)?,
                    "bytecode labels",
                )?;
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                count = checked_count_add(
                    count,
                    try_children_count(
                        block,
                        handler.as_ref(),
                        finalizer.as_deref(),
                        "bytecode labels",
                        statements_label_count,
                    )?,
                    "bytecode labels",
                )?;
                if handler.is_some() {
                    count = checked_count_add(count, 1, "bytecode labels")?;
                }
            }
            HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Empty => {}
        }
    }
    Ok(count)
}

/// Counts the condition, update, and exit labels plus labels nested in every loop component.
fn for_label_count(
    initializer: Option<&HirForInitializer>,
    test: Option<&HirExpression>,
    update: Option<&HirExpression>,
    body: &HirStatement,
) -> Result<usize, CompileError> {
    let mut count = 3;
    match initializer {
        Some(HirForInitializer::Variable(declaration)) => {
            for expression in declaration
                .declarators
                .iter()
                .filter_map(|declarator| declarator.initializer.as_ref())
            {
                count = checked_count_add(
                    count,
                    expression_label_count(expression)?,
                    "bytecode labels",
                )?;
            }
        }
        Some(HirForInitializer::Expression(expression)) => {
            count = checked_count_add(
                count,
                expression_label_count(expression)?,
                "bytecode labels",
            )?;
        }
        None => {}
    }
    for expression in [test, update].into_iter().flatten() {
        count = checked_count_add(
            count,
            expression_label_count(expression)?,
            "bytecode labels",
        )?;
    }
    checked_count_add(
        count,
        statements_label_count(core::slice::from_ref(body))?,
        "bytecode labels",
    )
}

fn for_in_left_label_count(left: &HirForInLeft) -> Result<usize, CompileError> {
    let HirForInLeft::Assignment(target) = left else {
        return Ok(0);
    };
    match target {
        HirAssignmentTarget::Identifier(_) => Ok(0),
        HirAssignmentTarget::StaticMember { object, .. } => expression_label_count(object),
        HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
            expression_label_count(object)?,
            expression_label_count(property)?,
            "bytecode labels",
        ),
    }
}

/// Reserves one label per clause, one shared end label, and every nested expression/body label.
fn switch_label_count(
    discriminant: &HirExpression,
    cases: &[HirSwitchCase],
) -> Result<usize, CompileError> {
    let mut count = expression_label_count(discriminant)?;
    count = checked_count_add(count, cases.len(), "bytecode labels")?;
    count = checked_count_add(count, 1, "bytecode labels")?;
    for case in cases {
        if let Some(test) = case.test.as_ref() {
            count = checked_count_add(count, expression_label_count(test)?, "bytecode labels")?;
        }
        count = checked_count_add(
            count,
            statements_label_count(&case.consequent)?,
            "bytecode labels",
        )?;
    }
    Ok(count)
}

/// Counts expression statements whose values may update a structured script completion.
pub(super) fn statements_expression_count(
    statements: &[HirStatement],
) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let statement_count = match &statement.kind {
            HirStatementKind::Expression(_) => 1,
            HirStatementKind::Block(statements) => statements_expression_count(statements)?,
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                let mut nested = statements_expression_count(core::slice::from_ref(consequent))?;
                if let Some(alternate) = alternate {
                    nested = checked_count_add(
                        nested,
                        statements_expression_count(core::slice::from_ref(alternate))?,
                        "entry completion instructions",
                    )?;
                }
                nested
            }
            HirStatementKind::For { body, .. }
            | HirStatementKind::ForIn { body, .. }
            | HirStatementKind::Loop { body, .. } => {
                statements_expression_count(core::slice::from_ref(body))?
            }
            HirStatementKind::Switch { cases, .. } => switch_expression_count(cases)?,
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => try_children_count(
                block,
                handler.as_ref(),
                finalizer.as_deref(),
                "entry completion instructions",
                statements_expression_count,
            )?,
            HirStatementKind::VariableDeclaration(_)
            | HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "entry completion instructions")?;
    }
    Ok(count)
}

/// Counts clause expression statements that can update script completion after dispatch.
fn switch_expression_count(cases: &[HirSwitchCase]) -> Result<usize, CompileError> {
    let mut count = 0;
    for case in cases {
        count = checked_count_add(
            count,
            statements_expression_count(&case.consequent)?,
            "entry completion instructions",
        )?;
    }
    Ok(count)
}

/// Counts switch and loop nodes as an exact-capacity upper bound for the active break-target stack.
pub(super) fn statements_switch_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let nested = match &statement.kind {
            HirStatementKind::Block(statements) => statements_switch_count(statements)?,
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                let mut count = statements_switch_count(core::slice::from_ref(consequent))?;
                if let Some(alternate) = alternate {
                    count = checked_count_add(
                        count,
                        statements_switch_count(core::slice::from_ref(alternate))?,
                        "switch control targets",
                    )?;
                }
                count
            }
            HirStatementKind::For { body, .. }
            | HirStatementKind::ForIn { body, .. }
            | HirStatementKind::Loop { body, .. } => checked_count_add(
                1,
                statements_switch_count(core::slice::from_ref(body))?,
                "switch control targets",
            )?,
            HirStatementKind::Switch { cases, .. } => {
                let mut count = 1;
                for case in cases.iter() {
                    count = checked_count_add(
                        count,
                        statements_switch_count(&case.consequent)?,
                        "switch control targets",
                    )?;
                }
                count
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => try_children_count(
                block,
                handler.as_ref(),
                finalizer.as_deref(),
                "switch control targets",
                statements_switch_count,
            )?,
            HirStatementKind::Expression(_)
            | HirStatementKind::VariableDeclaration(_)
            | HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, nested, "switch control targets")?;
    }
    Ok(count)
}

/// Counts loop nesting as an exact-capacity upper bound for the active continue-target stack.
pub(super) fn statements_loop_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let nested = match &statement.kind {
            HirStatementKind::Block(statements) => statements_loop_count(statements)?,
            HirStatementKind::If {
                consequent,
                alternate,
                ..
            } => {
                let mut nested = statements_loop_count(core::slice::from_ref(consequent))?;
                if let Some(alternate) = alternate {
                    nested = checked_count_add(
                        nested,
                        statements_loop_count(core::slice::from_ref(alternate))?,
                        "loop continue targets",
                    )?;
                }
                nested
            }
            HirStatementKind::For { body, .. }
            | HirStatementKind::ForIn { body, .. }
            | HirStatementKind::Loop { body, .. } => checked_count_add(
                1,
                statements_loop_count(core::slice::from_ref(body))?,
                "loop continue targets",
            )?,
            HirStatementKind::Switch { cases, .. } => {
                let mut nested = 0;
                for case in cases.iter() {
                    nested = checked_count_add(
                        nested,
                        statements_loop_count(&case.consequent)?,
                        "loop continue targets",
                    )?;
                }
                nested
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => try_children_count(
                block,
                handler.as_ref(),
                finalizer.as_deref(),
                "loop continue targets",
                statements_loop_count,
            )?,
            HirStatementKind::Expression(_)
            | HirStatementKind::VariableDeclaration(_)
            | HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Return(_)
            | HirStatementKind::Throw(_)
            | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, nested, "loop continue targets")?;
    }
    Ok(count)
}

/// Counts nested conditional arms, each of which consumes exactly two symbolic labels.
fn expression_label_count(expression: &HirExpression) -> Result<usize, CompileError> {
    match &expression.kind {
        HirExpressionKind::Sequence(expressions) => {
            let mut count = 0;
            for expression in expressions.iter() {
                count = checked_count_add(
                    count,
                    expression_label_count(expression)?,
                    "bytecode labels",
                )?;
            }
            Ok(count)
        }
        HirExpressionKind::Object(properties) | HirExpressionKind::Array(properties) => {
            let mut count = 0;
            for property in properties.iter() {
                count = checked_count_add(
                    count,
                    expression_label_count(&property.value)?,
                    "bytecode labels",
                )?;
                if let HirObjectPropertyKey::Computed(key) = &property.key {
                    count =
                        checked_count_add(count, expression_label_count(key)?, "bytecode labels")?;
                }
            }
            Ok(count)
        }
        HirExpressionKind::Binary { left, right, .. } => checked_count_add(
            expression_label_count(left)?,
            expression_label_count(right)?,
            "bytecode labels",
        ),
        HirExpressionKind::Logical { left, right, .. } => {
            let nested = checked_count_add(
                expression_label_count(left)?,
                expression_label_count(right)?,
                "bytecode labels",
            )?;
            checked_count_add(nested, 1, "bytecode labels")
        }
        HirExpressionKind::StaticMember { object, .. } => expression_label_count(object),
        HirExpressionKind::ComputedMember { object, property } => checked_count_add(
            expression_label_count(object)?,
            expression_label_count(property)?,
            "bytecode labels",
        ),
        HirExpressionKind::Assignment {
            operator,
            target,
            value,
        } => {
            let target = match target {
                HirAssignmentTarget::Identifier(_) => 0,
                HirAssignmentTarget::StaticMember { object, .. } => expression_label_count(object)?,
                HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
                    expression_label_count(object)?,
                    expression_label_count(property)?,
                    "bytecode labels",
                )?,
            };
            let nested =
                checked_count_add(target, expression_label_count(value)?, "bytecode labels")?;
            if matches!(operator, HirAssignmentOperator::Logical(_)) {
                checked_count_add(nested, 1, "bytecode labels")
            } else {
                Ok(nested)
            }
        }
        HirExpressionKind::Update { target, .. } => match target {
            HirAssignmentTarget::Identifier(_) => Ok(0),
            HirAssignmentTarget::StaticMember { object, .. } => expression_label_count(object),
            HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
                expression_label_count(object)?,
                expression_label_count(property)?,
                "bytecode labels",
            ),
        },
        HirExpressionKind::Unary { argument, .. } => expression_label_count(argument),
        HirExpressionKind::Conditional {
            test,
            consequent,
            alternate,
        } => {
            let nested = checked_count_add(
                expression_label_count(test)?,
                checked_count_add(
                    expression_label_count(consequent)?,
                    expression_label_count(alternate)?,
                    "bytecode labels",
                )?,
                "bytecode labels",
            )?;
            checked_count_add(nested, 2, "bytecode labels")
        }
        HirExpressionKind::Call { callee, arguments }
        | HirExpressionKind::New { callee, arguments } => {
            let mut count = expression_label_count(callee)?;
            for argument in arguments.iter() {
                count =
                    checked_count_add(count, expression_label_count(argument)?, "bytecode labels")?;
            }
            Ok(count)
        }
        _ => Ok(0),
    }
}
