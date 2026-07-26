use super::super::var_declared_bindings;
use super::{checked_count_add, try_children_count};
use crate::hir::{HirAssignmentTarget, HirForInLeft};
use crate::{
    CompileError, HirExpression, HirExpressionKind, HirForInitializer, HirObjectPropertyKey,
    HirProgram, HirStatement, HirStatementKind, HirSwitchCase, HirVariableDeclaration,
};

/// Computes a checked upper bound for module scope-name interning before HIR lowering starts.
pub(super) fn hir_scope_name_capacity(hir: &HirProgram) -> Result<usize, CompileError> {
    let mut count = checked_count_add(
        statements_scope_name_count(hir.statements())?,
        var_declared_bindings(hir.statements())?.len(),
        "scope names",
    )?;
    for function in hir.functions() {
        count = checked_count_add(
            count,
            statements_scope_name_count(&function.body)?,
            "scope names",
        )?;
        count = checked_count_add(
            count,
            parameter_scope_name_count(&function.parameter_initializers)?,
            "scope names",
        )?;
        count = checked_count_add(count, usize::from(function.name.is_some()), "scope names")?;
    }
    Ok(count)
}

/// Counts identifier references and published top-level function names across structured statements.
pub(super) fn statements_scope_name_count(
    statements: &[HirStatement],
) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let statement_count = match &statement.kind {
            HirStatementKind::Expression(expression) => expression_scope_name_count(expression)?,
            HirStatementKind::VariableDeclaration(declaration) => {
                let mut nested = 0;
                for initializer in declaration
                    .declarators
                    .iter()
                    .filter_map(|declarator| declarator.initializer.as_ref())
                {
                    nested = checked_count_add(
                        nested,
                        expression_scope_name_count(initializer)?,
                        "scope names",
                    )?;
                }
                nested
            }
            HirStatementKind::FunctionDeclaration(_) => 1,
            HirStatementKind::Block(statements) => statements_scope_name_count(statements)?,
            HirStatementKind::If {
                test,
                consequent,
                alternate,
            } => {
                let mut nested = expression_scope_name_count(test)?;
                nested = checked_count_add(
                    nested,
                    statements_scope_name_count(core::slice::from_ref(consequent))?,
                    "scope names",
                )?;
                if let Some(alternate) = alternate {
                    nested = checked_count_add(
                        nested,
                        statements_scope_name_count(core::slice::from_ref(alternate))?,
                        "scope names",
                    )?;
                }
                nested
            }
            HirStatementKind::For {
                initializer,
                test,
                update,
                body,
            } => for_scope_name_count(initializer.as_ref(), test.as_ref(), update.as_ref(), body)?,
            HirStatementKind::ForIn { left, right, body } => {
                let mut nested = expression_scope_name_count(right)?;
                nested =
                    checked_count_add(nested, for_in_left_scope_name_count(left)?, "scope names")?;
                checked_count_add(
                    nested,
                    statements_scope_name_count(core::slice::from_ref(body))?,
                    "scope names",
                )?
            }
            HirStatementKind::ForOf {
                left, right, body, ..
            } => {
                let mut nested = expression_scope_name_count(right)?;
                nested =
                    checked_count_add(nested, for_in_left_scope_name_count(left)?, "scope names")?;
                nested = checked_count_add(
                    nested,
                    statements_scope_name_count(core::slice::from_ref(body))?,
                    "scope names",
                )?;
                // Symbol, iterator, next, done, value, and return.
                checked_count_add(nested, 6, "scope names")?
            }
            HirStatementKind::Loop { test, body, .. } => checked_count_add(
                expression_scope_name_count(test)?,
                statements_scope_name_count(core::slice::from_ref(body))?,
                "scope names",
            )?,
            HirStatementKind::Switch {
                discriminant,
                cases,
            } => switch_scope_name_count(discriminant, cases)?,
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => try_children_count(
                block,
                handler.as_ref(),
                finalizer.as_deref(),
                "scope names",
                statements_scope_name_count,
            )?,
            HirStatementKind::Return(argument) => argument
                .as_ref()
                .map(expression_scope_name_count)
                .transpose()?
                .unwrap_or(0),
            HirStatementKind::Throw(argument) => expression_scope_name_count(argument)?,
            HirStatementKind::Break | HirStatementKind::Continue | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "scope names")?;
    }
    Ok(count)
}

/// Counts every for-loop expression and body reference without flattening evaluation order.
fn for_scope_name_count(
    initializer: Option<&HirForInitializer>,
    test: Option<&HirExpression>,
    update: Option<&HirExpression>,
    body: &HirStatement,
) -> Result<usize, CompileError> {
    let mut count = match initializer {
        Some(HirForInitializer::Variable(declaration)) => {
            declaration_scope_name_count(declaration)?
        }
        Some(HirForInitializer::Expression(expression)) => expression_scope_name_count(expression)?,
        None => 0,
    };
    for expression in [test, update].into_iter().flatten() {
        count = checked_count_add(
            count,
            expression_scope_name_count(expression)?,
            "scope names",
        )?;
    }
    checked_count_add(
        count,
        statements_scope_name_count(core::slice::from_ref(body))?,
        "scope names",
    )
}

/// Counts identifier references in every declaration initializer.
fn declaration_scope_name_count(
    declaration: &HirVariableDeclaration,
) -> Result<usize, CompileError> {
    let mut count = 0;
    for initializer in declaration
        .declarators
        .iter()
        .filter_map(|declarator| declarator.initializer.as_ref())
    {
        count = checked_count_add(
            count,
            expression_scope_name_count(initializer)?,
            "scope names",
        )?;
    }
    Ok(count)
}

fn for_in_left_scope_name_count(left: &HirForInLeft) -> Result<usize, CompileError> {
    match left {
        HirForInLeft::Variable(_) => Ok(1),
        HirForInLeft::Assignment(target) => target
            .assignment_target()
            .map_or(Ok(0), assignment_target_scope_name_count),
    }
}

fn assignment_target_scope_name_count(target: &HirAssignmentTarget) -> Result<usize, CompileError> {
    match target {
        HirAssignmentTarget::Identifier(_) => Ok(1),
        HirAssignmentTarget::StaticMember { object, .. } => {
            checked_count_add(expression_scope_name_count(object)?, 1, "scope names")
        }
        HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
            expression_scope_name_count(object)?,
            expression_scope_name_count(property)?,
            "scope names",
        ),
        HirAssignmentTarget::PrivateMember { object, .. } => expression_scope_name_count(object),
    }
}

/// Counts discriminant, case-test, and clause-body identifiers without flattening case order.
fn switch_scope_name_count(
    discriminant: &HirExpression,
    cases: &[HirSwitchCase],
) -> Result<usize, CompileError> {
    let mut count = expression_scope_name_count(discriminant)?;
    for case in cases {
        if let Some(test) = case.test.as_ref() {
            count = checked_count_add(count, expression_scope_name_count(test)?, "scope names")?;
        }
        count = checked_count_add(
            count,
            statements_scope_name_count(&case.consequent)?,
            "scope names",
        )?;
    }
    Ok(count)
}

/// Counts every identifier occurrence in one expression as a conservative scope-name upper bound.
fn expression_scope_name_count(expression: &HirExpression) -> Result<usize, CompileError> {
    match &expression.kind {
        HirExpressionKind::Identifier(_) => Ok(1),
        HirExpressionKind::Class(class) => {
            let mut count = class
                .super_class
                .as_deref()
                .map(expression_scope_name_count)
                .transpose()?
                .unwrap_or(0);
            count = checked_count_add(count, 1, "scope names")?;
            if class.name.is_some() {
                count = checked_count_add(count, 1, "scope names")?;
            }
            count = checked_count_add(count, class.private_names.len(), "scope names")?;
            for element in class.elements.iter() {
                let key = match element {
                    crate::HirClassElement::Method(method) => Some(&method.key),
                    crate::HirClassElement::PublicField(field) => Some(&field.key),
                    crate::HirClassElement::PrivateField(_) => None,
                    crate::HirClassElement::PrivateMethod(_) => None,
                    crate::HirClassElement::PrivateAccessor(_) => None,
                    crate::HirClassElement::StaticBlock(_) => None,
                };
                if let Some(key) = key {
                    count = checked_count_add(
                        count,
                        match key {
                            HirObjectPropertyKey::Static(_) => 1,
                            HirObjectPropertyKey::Computed(key) => {
                                expression_scope_name_count(key)?
                            }
                        },
                        "scope names",
                    )?;
                }
            }
            Ok(count)
        }
        HirExpressionKind::SuperStaticMember(_) => Ok(1),
        HirExpressionKind::SuperComputedMember(property) => expression_scope_name_count(property),
        HirExpressionKind::Yield(argument) => argument
            .as_deref()
            .map(expression_scope_name_count)
            .transpose()
            .map(|count| count.unwrap_or(0)),
        HirExpressionKind::Sequence(expressions) => {
            let mut count = 0;
            for expression in expressions.iter() {
                count = checked_count_add(
                    count,
                    expression_scope_name_count(expression)?,
                    "scope names",
                )?;
            }
            Ok(count)
        }
        HirExpressionKind::Object(properties) | HirExpressionKind::Array(properties) => {
            let mut count = 0;
            for property in properties.iter() {
                count = checked_count_add(
                    count,
                    object_property_scope_name_count(&property.value)?,
                    "scope names",
                )?;
                if matches!(property.key, HirObjectPropertyKey::Static(_)) {
                    count = checked_count_add(count, 1, "scope names")?;
                }
                if matches!(
                    property.value,
                    crate::HirObjectPropertyValue::Getter(_)
                        | crate::HirObjectPropertyValue::Setter(_)
                ) {
                    count = checked_count_add(count, 1, "scope names")?;
                }
                if let HirObjectPropertyKey::Computed(key) = &property.key {
                    count =
                        checked_count_add(count, expression_scope_name_count(key)?, "scope names")?;
                }
            }
            Ok(count)
        }
        HirExpressionKind::ObjectSpread(parts) => {
            let mut count = 0;
            for part in parts.iter() {
                let nested = match part {
                    crate::hir::HirObjectExpressionPart::Property(property) => {
                        let mut count = object_property_scope_name_count(&property.value)?;
                        if let HirObjectPropertyKey::Computed(key) = &property.key {
                            count = checked_count_add(
                                count,
                                expression_scope_name_count(key)?,
                                "scope names",
                            )?;
                        }
                        count
                    }
                    crate::hir::HirObjectExpressionPart::Spread(source) => {
                        expression_scope_name_count(source)?
                    }
                };
                count = checked_count_add(count, nested, "scope names")?;
            }
            Ok(count)
        }
        HirExpressionKind::ArrayAccumulation(parts) => {
            let mut count = 4;
            for part in parts.iter() {
                let expression = match part {
                    crate::hir::HirArrayExpressionPart::Element(expression)
                    | crate::hir::HirArrayExpressionPart::Spread(expression) => expression,
                    crate::hir::HirArrayExpressionPart::Elision => continue,
                };
                count = checked_count_add(
                    count,
                    expression_scope_name_count(expression)?,
                    "scope names",
                )?;
            }
            Ok(count)
        }
        HirExpressionKind::StaticMember { object, .. } => {
            checked_count_add(expression_scope_name_count(object)?, 1, "scope names")
        }
        HirExpressionKind::ComputedMember { object, property } => checked_count_add(
            expression_scope_name_count(object)?,
            expression_scope_name_count(property)?,
            "scope names",
        ),
        HirExpressionKind::PrivateMember { object, .. } => expression_scope_name_count(object),
        HirExpressionKind::PrivateIn { object, .. } => expression_scope_name_count(object),
        HirExpressionKind::Unary { argument, .. } => expression_scope_name_count(argument),
        HirExpressionKind::Binary { left, right, .. } => checked_count_add(
            expression_scope_name_count(left)?,
            expression_scope_name_count(right)?,
            "scope names",
        ),
        HirExpressionKind::Logical { left, right, .. } => checked_count_add(
            expression_scope_name_count(left)?,
            expression_scope_name_count(right)?,
            "scope names",
        ),
        HirExpressionKind::Assignment { target, value, .. } => {
            let Some(target) = target.assignment_target() else {
                return expression_scope_name_count(value);
            };
            let target = match target {
                HirAssignmentTarget::Identifier(_) => 1,
                HirAssignmentTarget::StaticMember { object, .. } => {
                    checked_count_add(expression_scope_name_count(object)?, 1, "scope names")?
                }
                HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
                    expression_scope_name_count(object)?,
                    expression_scope_name_count(property)?,
                    "scope names",
                )?,
                HirAssignmentTarget::PrivateMember { object, .. } => {
                    expression_scope_name_count(object)?
                }
            };
            checked_count_add(target, expression_scope_name_count(value)?, "scope names")
        }
        HirExpressionKind::Update { target, .. } => match target {
            HirAssignmentTarget::Identifier(_) => Ok(1),
            HirAssignmentTarget::StaticMember { object, .. } => {
                checked_count_add(expression_scope_name_count(object)?, 1, "scope names")
            }
            HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
                expression_scope_name_count(object)?,
                expression_scope_name_count(property)?,
                "scope names",
            ),
            HirAssignmentTarget::PrivateMember { object, .. } => {
                expression_scope_name_count(object)
            }
        },
        HirExpressionKind::Conditional {
            test,
            consequent,
            alternate,
        } => checked_count_add(
            expression_scope_name_count(test)?,
            checked_count_add(
                expression_scope_name_count(consequent)?,
                expression_scope_name_count(alternate)?,
                "scope names",
            )?,
            "scope names",
        ),
        HirExpressionKind::Call { callee, arguments }
        | HirExpressionKind::New { callee, arguments } => {
            let mut count = expression_scope_name_count(callee)?;
            for argument in arguments.iter() {
                count = checked_count_add(
                    count,
                    expression_scope_name_count(argument)?,
                    "scope names",
                )?;
            }
            Ok(count)
        }
        HirExpressionKind::SuperCall(arguments) => {
            let mut count = 0;
            for argument in arguments.iter() {
                count = checked_count_add(
                    count,
                    expression_scope_name_count(argument)?,
                    "scope names",
                )?;
            }
            Ok(count)
        }
        HirExpressionKind::Number(_)
        | HirExpressionKind::String(_)
        | HirExpressionKind::RegExp { .. }
        | HirExpressionKind::Boolean(_)
        | HirExpressionKind::Null
        | HirExpressionKind::Function(_)
        | HirExpressionKind::This
        | HirExpressionKind::NewTarget => Ok(0),
    }
}

pub(super) fn hir_literal_count(hir: &HirProgram) -> Result<usize, CompileError> {
    let mut count = statements_literal_count(hir.statements())?;
    for function in hir.functions() {
        count = checked_count_add(
            count,
            statements_literal_count(&function.body)?,
            "bytecode constants",
        )?;
        count = checked_count_add(
            count,
            parameter_literal_count(&function.parameter_initializers)?,
            "bytecode constants",
        )?;
    }
    Ok(count)
}

pub(super) fn parameter_scope_name_count(
    initializers: &[Option<HirExpression>],
) -> Result<usize, CompileError> {
    let mut count = 0;
    for initializer in initializers.iter().flatten() {
        count = checked_count_add(
            count,
            expression_scope_name_count(initializer)?,
            "scope names",
        )?;
    }
    Ok(count)
}

fn parameter_literal_count(initializers: &[Option<HirExpression>]) -> Result<usize, CompileError> {
    let mut count = 0;
    for initializer in initializers.iter().flatten() {
        count = checked_count_add(
            count,
            expression_literal_count(initializer)?,
            "bytecode constants",
        )?;
    }
    Ok(count)
}

/// Counts literal constants recursively before the module-wide pool is allocated.
fn statements_literal_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let statement_count = match &statement.kind {
            HirStatementKind::Expression(expression) => expression_literal_count(expression)?,
            HirStatementKind::VariableDeclaration(declaration) => {
                declaration_literal_count(declaration)?
            }
            HirStatementKind::Return(argument) => argument
                .as_ref()
                .map(expression_literal_count)
                .transpose()?
                .unwrap_or(0),
            HirStatementKind::Throw(argument) => expression_literal_count(argument)?,
            HirStatementKind::Block(statements) => statements_literal_count(statements)?,
            HirStatementKind::If {
                test,
                consequent,
                alternate,
            } => {
                let mut count = expression_literal_count(test)?;
                count = checked_count_add(
                    count,
                    statements_literal_count(core::slice::from_ref(consequent))?,
                    "bytecode constants",
                )?;
                if let Some(alternate) = alternate {
                    count = checked_count_add(
                        count,
                        statements_literal_count(core::slice::from_ref(alternate))?,
                        "bytecode constants",
                    )?;
                }
                count
            }
            HirStatementKind::For {
                initializer,
                test,
                update,
                body,
            } => for_literal_count(initializer.as_ref(), test.as_ref(), update.as_ref(), body)?,
            HirStatementKind::ForIn { left, right, body } => {
                let mut nested = expression_literal_count(right)?;
                nested = checked_count_add(
                    nested,
                    for_in_left_literal_count(left)?,
                    "bytecode constants",
                )?;
                checked_count_add(
                    nested,
                    statements_literal_count(core::slice::from_ref(body))?,
                    "bytecode constants",
                )?
            }
            HirStatementKind::ForOf {
                left, right, body, ..
            } => {
                let mut nested = expression_literal_count(right)?;
                nested = checked_count_add(
                    nested,
                    for_in_left_literal_count(left)?,
                    "bytecode constants",
                )?;
                checked_count_add(
                    nested,
                    statements_literal_count(core::slice::from_ref(body))?,
                    "bytecode constants",
                )?
            }
            HirStatementKind::Loop { test, body, .. } => checked_count_add(
                expression_literal_count(test)?,
                statements_literal_count(core::slice::from_ref(body))?,
                "bytecode constants",
            )?,
            HirStatementKind::Switch {
                discriminant,
                cases,
            } => switch_literal_count(discriminant, cases)?,
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => try_children_count(
                block,
                handler.as_ref(),
                finalizer.as_deref(),
                "bytecode constants",
                statements_literal_count,
            )?,
            HirStatementKind::FunctionDeclaration(_) => 0,
            HirStatementKind::Break | HirStatementKind::Continue | HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "bytecode constants")?;
    }
    Ok(count)
}

/// Counts constants in each loop component without counting the synthetic update value one.
fn for_literal_count(
    initializer: Option<&HirForInitializer>,
    test: Option<&HirExpression>,
    update: Option<&HirExpression>,
    body: &HirStatement,
) -> Result<usize, CompileError> {
    let mut count = match initializer {
        Some(HirForInitializer::Variable(declaration)) => declaration_literal_count(declaration)?,
        Some(HirForInitializer::Expression(expression)) => expression_literal_count(expression)?,
        None => 0,
    };
    for expression in [test, update].into_iter().flatten() {
        count = checked_count_add(
            count,
            expression_literal_count(expression)?,
            "bytecode constants",
        )?;
    }
    checked_count_add(
        count,
        statements_literal_count(core::slice::from_ref(body))?,
        "bytecode constants",
    )
}

fn for_in_left_literal_count(left: &HirForInLeft) -> Result<usize, CompileError> {
    let HirForInLeft::Assignment(pattern) = left else {
        return Ok(0);
    };
    let Some(target) = pattern.assignment_target() else {
        return Ok(0);
    };
    match target {
        HirAssignmentTarget::Identifier(_) => Ok(0),
        HirAssignmentTarget::StaticMember { object, .. } => expression_literal_count(object),
        HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
            expression_literal_count(object)?,
            expression_literal_count(property)?,
            "bytecode constants",
        ),
        HirAssignmentTarget::PrivateMember { object, .. } => expression_literal_count(object),
    }
}

/// Counts constants in both dispatch expressions and source-ordered clause bodies.
fn switch_literal_count(
    discriminant: &HirExpression,
    cases: &[HirSwitchCase],
) -> Result<usize, CompileError> {
    let mut count = expression_literal_count(discriminant)?;
    for case in cases {
        if let Some(test) = case.test.as_ref() {
            count =
                checked_count_add(count, expression_literal_count(test)?, "bytecode constants")?;
        }
        count = checked_count_add(
            count,
            statements_literal_count(&case.consequent)?,
            "bytecode constants",
        )?;
    }
    Ok(count)
}

fn declaration_literal_count(declaration: &HirVariableDeclaration) -> Result<usize, CompileError> {
    let mut count = 0;
    for declarator in declaration.declarators.iter() {
        let initializer_count = declarator
            .initializer
            .as_ref()
            .map(expression_literal_count)
            .transpose()?
            .unwrap_or(0);
        count = checked_count_add(count, initializer_count, "bytecode constants")?;
    }
    Ok(count)
}

fn expression_literal_count(expression: &HirExpression) -> Result<usize, CompileError> {
    match &expression.kind {
        HirExpressionKind::Class(class) => {
            let mut count = class
                .super_class
                .as_deref()
                .map(expression_literal_count)
                .transpose()?
                .unwrap_or(0);
            for element in class.elements.iter() {
                let key = match element {
                    crate::HirClassElement::Method(method) => Some(&method.key),
                    crate::HirClassElement::PublicField(field) => Some(&field.key),
                    crate::HirClassElement::PrivateField(_) => None,
                    crate::HirClassElement::PrivateMethod(_) => None,
                    crate::HirClassElement::PrivateAccessor(_) => None,
                    crate::HirClassElement::StaticBlock(_) => None,
                };
                if let Some(HirObjectPropertyKey::Computed(key)) = key {
                    count = checked_count_add(
                        count,
                        expression_literal_count(key)?,
                        "bytecode constants",
                    )?;
                }
            }
            Ok(count)
        }
        HirExpressionKind::SuperComputedMember(property) => expression_literal_count(property),
        HirExpressionKind::Sequence(expressions) => {
            let mut count = 0;
            for expression in expressions.iter() {
                count = checked_count_add(
                    count,
                    expression_literal_count(expression)?,
                    "bytecode constants",
                )?;
            }
            Ok(count)
        }
        HirExpressionKind::Object(properties) | HirExpressionKind::Array(properties) => {
            let mut count = 0;
            for property in properties.iter() {
                count = checked_count_add(
                    count,
                    object_property_literal_count(&property.value)?,
                    "bytecode constants",
                )?;
                if let HirObjectPropertyKey::Computed(key) = &property.key {
                    count = checked_count_add(
                        count,
                        expression_literal_count(key)?,
                        "bytecode constants",
                    )?;
                }
            }
            Ok(count)
        }
        HirExpressionKind::ObjectSpread(parts) => {
            let mut count = 0;
            for part in parts.iter() {
                let nested = match part {
                    crate::hir::HirObjectExpressionPart::Property(property) => {
                        let mut count = object_property_literal_count(&property.value)?;
                        if let HirObjectPropertyKey::Computed(key) = &property.key {
                            count = checked_count_add(
                                count,
                                expression_literal_count(key)?,
                                "bytecode constants",
                            )?;
                        }
                        count
                    }
                    crate::hir::HirObjectExpressionPart::Spread(source) => {
                        expression_literal_count(source)?
                    }
                };
                count = checked_count_add(count, nested, "bytecode constants")?;
            }
            Ok(count)
        }
        HirExpressionKind::Number(_) => Ok(1),
        HirExpressionKind::Binary { left, right, .. } => checked_count_add(
            expression_literal_count(left)?,
            expression_literal_count(right)?,
            "bytecode constants",
        ),
        HirExpressionKind::Logical { left, right, .. } => checked_count_add(
            expression_literal_count(left)?,
            expression_literal_count(right)?,
            "bytecode constants",
        ),
        HirExpressionKind::StaticMember { object, .. } => expression_literal_count(object),
        HirExpressionKind::ComputedMember { object, property } => checked_count_add(
            expression_literal_count(object)?,
            expression_literal_count(property)?,
            "bytecode constants",
        ),
        HirExpressionKind::PrivateMember { object, .. } => expression_literal_count(object),
        HirExpressionKind::PrivateIn { object, .. } => expression_literal_count(object),
        HirExpressionKind::Assignment { target, value, .. } => {
            let Some(target) = target.assignment_target() else {
                return expression_literal_count(value);
            };
            let target = match target {
                HirAssignmentTarget::Identifier(_) => 0,
                HirAssignmentTarget::StaticMember { object, .. } => {
                    expression_literal_count(object)?
                }
                HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
                    expression_literal_count(object)?,
                    expression_literal_count(property)?,
                    "bytecode constants",
                )?,
                HirAssignmentTarget::PrivateMember { object, .. } => {
                    expression_literal_count(object)?
                }
            };
            checked_count_add(
                target,
                expression_literal_count(value)?,
                "bytecode constants",
            )
        }
        HirExpressionKind::Update { target, .. } => match target {
            HirAssignmentTarget::Identifier(_) => Ok(0),
            HirAssignmentTarget::StaticMember { object, .. } => expression_literal_count(object),
            HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
                expression_literal_count(object)?,
                expression_literal_count(property)?,
                "bytecode constants",
            ),
            HirAssignmentTarget::PrivateMember { object, .. } => expression_literal_count(object),
        },
        HirExpressionKind::Unary { argument, .. } => expression_literal_count(argument),
        HirExpressionKind::Conditional {
            test,
            consequent,
            alternate,
        } => checked_count_add(
            expression_literal_count(test)?,
            checked_count_add(
                expression_literal_count(consequent)?,
                expression_literal_count(alternate)?,
                "bytecode constants",
            )?,
            "bytecode constants",
        ),
        HirExpressionKind::Call { callee, arguments }
        | HirExpressionKind::New { callee, arguments } => {
            let mut count = expression_literal_count(callee)?;
            for argument in arguments.iter() {
                count = checked_count_add(
                    count,
                    expression_literal_count(argument)?,
                    "bytecode constants",
                )?;
            }
            Ok(count)
        }
        _ => Ok(0),
    }
}

fn object_property_scope_name_count(
    value: &crate::HirObjectPropertyValue,
) -> Result<usize, CompileError> {
    match value {
        crate::HirObjectPropertyValue::Data(expression) => expression_scope_name_count(expression),
        crate::HirObjectPropertyValue::Getter(_) | crate::HirObjectPropertyValue::Setter(_) => {
            Ok(0)
        }
    }
}

fn object_property_literal_count(
    value: &crate::HirObjectPropertyValue,
) -> Result<usize, CompileError> {
    match value {
        crate::HirObjectPropertyValue::Data(expression) => expression_literal_count(expression),
        crate::HirObjectPropertyValue::Getter(_) | crate::HirObjectPropertyValue::Setter(_) => {
            Ok(0)
        }
    }
}
