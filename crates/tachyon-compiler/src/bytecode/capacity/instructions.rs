use super::{checked_count_add, try_children_count};
use crate::hir::{HirAssignmentOperator, HirAssignmentTarget, HirForInLeft};
use crate::{
    CompileError, HirBinaryOperator, HirExpression, HirExpressionKind, HirForInitializer,
    HirObjectPropertyKey, HirPattern, HirPatternKind, HirProgram, HirStatement, HirStatementKind,
    HirSwitchCase, HirVariableDeclaration, HirVariableDeclarationKind,
};

pub(super) fn hir_instruction_count(hir: &HirProgram) -> Result<usize, CompileError> {
    statements_instruction_count(hir.statements())
}

/// Computes a checked instruction upper bound across nested structured statements.
pub(super) fn statements_instruction_count(
    statements: &[HirStatement],
) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let statement_count = match &statement.kind {
            HirStatementKind::Expression(expression) => expression_instruction_count(expression)?,
            HirStatementKind::VariableDeclaration(declaration) => {
                declaration_instruction_count(declaration)?
            }
            HirStatementKind::FunctionDeclaration(_) => 2,
            HirStatementKind::Return(argument) => argument
                .as_ref()
                .map(expression_instruction_count)
                .transpose()?
                .unwrap_or(1)
                .checked_add(1)
                .ok_or(CompileError::LoweringCapacityOverflow {
                    collection: "bytecode instructions",
                })?,
            HirStatementKind::Throw(argument) => checked_count_add(
                expression_instruction_count(argument)?,
                1,
                "bytecode instructions",
            )?,
            HirStatementKind::Block(statements) => statements_instruction_count(statements)?,
            HirStatementKind::Labeled { body, .. } => checked_count_add(
                1,
                statements_instruction_count(core::slice::from_ref(body))?,
                "bytecode instructions",
            )?,
            HirStatementKind::If {
                test,
                consequent,
                alternate,
            } => {
                let mut count = expression_instruction_count(test)?;
                count = checked_count_add(count, 2, "bytecode instructions")?;
                count = checked_count_add(
                    count,
                    statements_instruction_count(core::slice::from_ref(consequent))?,
                    "bytecode instructions",
                )?;
                if let Some(alternate) = alternate {
                    count = checked_count_add(
                        count,
                        statements_instruction_count(core::slice::from_ref(alternate))?,
                        "bytecode instructions",
                    )?;
                }
                count
            }
            HirStatementKind::For {
                initializer,
                test,
                update,
                body,
            } => for_instruction_count(initializer.as_ref(), test.as_ref(), update.as_ref(), body)?,
            HirStatementKind::ForIn { left, right, body } => {
                let mut nested = expression_instruction_count(right)?;
                nested = checked_count_add(
                    nested,
                    for_in_left_instruction_count(left)?,
                    "bytecode instructions",
                )?;
                nested = checked_count_add(
                    nested,
                    statements_instruction_count(core::slice::from_ref(body))?,
                    "bytecode instructions",
                )?;
                checked_count_add(nested, 8, "bytecode instructions")?
            }
            HirStatementKind::ForOf {
                r#await,
                left,
                right,
                body,
            } => {
                let mut nested = expression_instruction_count(right)?;
                nested = checked_count_add(
                    nested,
                    for_in_left_instruction_count(left)?,
                    "bytecode instructions",
                )?;
                nested = checked_count_add(
                    nested,
                    statements_instruction_count(core::slice::from_ref(body))?,
                    "bytecode instructions",
                )?;
                // Async iteration includes acquisition, the normalized Await loop, and close Await.
                let lowering_overhead = if *r#await { 60 } else { 24 };
                checked_count_add(nested, lowering_overhead, "bytecode instructions")?
            }
            HirStatementKind::Loop {
                test,
                body,
                test_first,
            } => {
                let mut nested = expression_instruction_count(test)?;
                nested = checked_count_add(
                    nested,
                    statements_instruction_count(core::slice::from_ref(body))?,
                    "bytecode instructions",
                )?;
                checked_count_add(
                    nested,
                    1 + usize::from(*test_first),
                    "bytecode instructions",
                )?
            }
            HirStatementKind::Switch {
                discriminant,
                cases,
            } => switch_instruction_count(discriminant, cases)?,
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                let nested = try_children_count(
                    block,
                    handler.as_ref(),
                    finalizer.as_deref(),
                    "bytecode instructions",
                    statements_instruction_count,
                )?;
                let control = if finalizer.is_some() {
                    4 + usize::from(handler.is_some()) * 2
                } else if handler.is_some() {
                    3
                } else {
                    0
                };
                checked_count_add(nested, control, "bytecode instructions")?
            }
            HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::BreakLabeled(_)
            | HirStatementKind::ContinueLabeled(_) => 1,
            HirStatementKind::Empty => 0,
        };
        count = checked_count_add(count, statement_count, "bytecode instructions")?;
    }
    Ok(count)
}

/// Counts classic for-loop evaluation plus its conditional and back-edge instructions exactly.
fn for_instruction_count(
    initializer: Option<&HirForInitializer>,
    test: Option<&HirExpression>,
    update: Option<&HirExpression>,
    body: &HirStatement,
) -> Result<usize, CompileError> {
    let mut count = match initializer {
        Some(HirForInitializer::Variable(declaration)) => {
            declaration_instruction_count(declaration)?
        }
        Some(HirForInitializer::Expression(expression)) => {
            expression_instruction_count(expression)?
        }
        None => 0,
    };
    if let Some(test) = test {
        count = checked_count_add(
            count,
            expression_instruction_count(test)?,
            "bytecode instructions",
        )?;
        count = checked_count_add(count, 1, "bytecode instructions")?;
    }
    if let Some(update) = update {
        count = checked_count_add(
            count,
            expression_instruction_count(update)?,
            "bytecode instructions",
        )?;
    }
    count = checked_count_add(
        count,
        statements_instruction_count(core::slice::from_ref(body))?,
        "bytecode instructions",
    )?;
    checked_count_add(count, 1, "bytecode instructions")
}

fn for_in_left_instruction_count(left: &HirForInLeft) -> Result<usize, CompileError> {
    match left {
        HirForInLeft::Variable(declaration) => {
            let pattern = &declaration
                .declarators
                .first()
                .expect("HIR validates one for-in declarator")
                .pattern;
            checked_count_add(
                pattern_binding_count(pattern)?,
                declared_pattern_instruction_count(pattern)?,
                "bytecode instructions",
            )
        }
        HirForInLeft::Assignment(pattern) => match pattern.assignment_target() {
            None => Ok(0),
            Some(HirAssignmentTarget::Identifier(_)) => Ok(1),
            Some(HirAssignmentTarget::StaticMember { object, .. }) => checked_count_add(
                expression_instruction_count(object)?,
                1,
                "bytecode instructions",
            ),
            Some(HirAssignmentTarget::ComputedMember { object, property }) => {
                let nested = checked_count_add(
                    expression_instruction_count(object)?,
                    expression_instruction_count(property)?,
                    "bytecode instructions",
                )?;
                checked_count_add(nested, 2, "bytecode instructions")
            }
            Some(HirAssignmentTarget::PrivateMember { object, .. }) => checked_count_add(
                expression_instruction_count(object)?,
                2,
                "bytecode instructions",
            ),
        },
    }
}

/// Mirrors recursive declaration-pattern lowering, including both emitted array branches.
fn declared_pattern_instruction_count(pattern: &HirPattern) -> Result<usize, CompileError> {
    match &pattern.kind {
        HirPatternKind::Binding(_) => Ok(1),
        HirPatternKind::Assignment(_) => Ok(0),
        HirPatternKind::Default {
            target,
            initializer,
        } => {
            let nested = checked_count_add(
                expression_instruction_count(initializer)?,
                declared_pattern_instruction_count(target)?,
                "bytecode instructions",
            )?;
            checked_count_add(nested, 7, "bytecode instructions")
        }
        HirPatternKind::Object { properties, rest } => {
            let mut count = 2 + usize::from(rest.is_some());
            for property in properties.iter() {
                let key = match &property.key {
                    HirObjectPropertyKey::Static(_) => 2,
                    HirObjectPropertyKey::Computed(expression) => checked_count_add(
                        expression_instruction_count(expression)?,
                        2,
                        "bytecode instructions",
                    )?,
                };
                count = checked_count_add(count, key, "bytecode instructions")?;
                count =
                    checked_count_add(count, usize::from(rest.is_some()), "bytecode instructions")?;
                count = checked_count_add(
                    count,
                    declared_pattern_instruction_count(&property.target)?,
                    "bytecode instructions",
                )?;
            }
            if let Some(rest) = rest {
                count = checked_count_add(count, 2, "bytecode instructions")?;
                count = checked_count_add(
                    count,
                    declared_pattern_instruction_count(rest)?,
                    "bytecode instructions",
                )?;
            }
            Ok(count)
        }
        HirPatternKind::Array { elements, rest } => {
            let mut count = 17;
            for element in elements.iter() {
                count = checked_count_add(count, 7, "bytecode instructions")?;
                if let Some(element) = element {
                    let branch = declared_pattern_instruction_count(element)?;
                    count = checked_count_add(count, 3, "bytecode instructions")?;
                    count = checked_count_add(
                        count,
                        branch
                            .checked_mul(2)
                            .ok_or(CompileError::LoweringCapacityOverflow {
                                collection: "bytecode instructions",
                            })?,
                        "bytecode instructions",
                    )?;
                }
            }
            if let Some(rest) = rest {
                count = checked_count_add(count, 15, "bytecode instructions")?;
                count = checked_count_add(
                    count,
                    declared_pattern_instruction_count(rest)?,
                    "bytecode instructions",
                )?;
            }
            Ok(count)
        }
    }
}

/// Counts declaration leaves initialized to undefined before the loop starts.
fn pattern_binding_count(pattern: &HirPattern) -> Result<usize, CompileError> {
    match &pattern.kind {
        HirPatternKind::Binding(_) => Ok(1),
        HirPatternKind::Assignment(_) => Ok(0),
        HirPatternKind::Default { target, .. } => pattern_binding_count(target),
        HirPatternKind::Array { elements, rest } => {
            let mut count = 0;
            for element in elements.iter().flatten() {
                count = checked_count_add(
                    count,
                    pattern_binding_count(element)?,
                    "bytecode instructions",
                )?;
            }
            if let Some(rest) = rest {
                count = checked_count_add(
                    count,
                    pattern_binding_count(rest)?,
                    "bytecode instructions",
                )?;
            }
            Ok(count)
        }
        HirPatternKind::Object { properties, rest } => {
            let mut count = 0;
            for property in properties.iter() {
                count = checked_count_add(
                    count,
                    pattern_binding_count(&property.target)?,
                    "bytecode instructions",
                )?;
            }
            if let Some(rest) = rest {
                count = checked_count_add(
                    count,
                    pattern_binding_count(rest)?,
                    "bytecode instructions",
                )?;
            }
            Ok(count)
        }
    }
}

/// Includes dispatch comparisons, conditional branches, fallback jump, and every clause body.
fn switch_instruction_count(
    discriminant: &HirExpression,
    cases: &[HirSwitchCase],
) -> Result<usize, CompileError> {
    let mut count = checked_count_add(
        expression_instruction_count(discriminant)?,
        2,
        "bytecode instructions",
    )?;
    for case in cases {
        if let Some(test) = case.test.as_ref() {
            count = checked_count_add(
                count,
                expression_instruction_count(test)?,
                "bytecode instructions",
            )?;
            count = checked_count_add(count, 2, "bytecode instructions")?;
        }
        count = checked_count_add(
            count,
            statements_instruction_count(&case.consequent)?,
            "bytecode instructions",
        )?;
    }
    Ok(count)
}

fn declaration_instruction_count(
    declaration: &HirVariableDeclaration,
) -> Result<usize, CompileError> {
    let mut count = 0;
    for declarator in declaration.declarators.iter() {
        let initializer_count = match declarator.initializer.as_ref() {
            Some(initializer) => {
                let count = expression_instruction_count(initializer)?;
                if declaration.kind == HirVariableDeclarationKind::Var {
                    checked_count_add(count, 1, "bytecode instructions")?
                } else {
                    count
                }
            }
            None if declaration.kind == HirVariableDeclarationKind::Var => 0,
            None => 1,
        };
        count = checked_count_add(count, initializer_count, "bytecode instructions")?;
    }
    Ok(count)
}

fn expression_instruction_count(expression: &HirExpression) -> Result<usize, CompileError> {
    match &expression.kind {
        HirExpressionKind::Class(class) => {
            let mut element_instructions = 0;
            for element in class.elements.iter() {
                let (key, fixed) = match element {
                    crate::HirClassElement::Method(method) => (Some(&method.key), 2),
                    crate::HirClassElement::PublicField(field) => (Some(&field.key), 7),
                    crate::HirClassElement::PrivateField(_) => (None, 7),
                    crate::HirClassElement::PrivateMethod(method) => {
                        (None, if method.is_static { 4 } else { 7 })
                    }
                    crate::HirClassElement::PrivateAccessor(accessor) => {
                        (None, if accessor.is_static { 7 } else { 10 })
                    }
                    crate::HirClassElement::StaticBlock(_) => (None, 5),
                };
                element_instructions =
                    checked_count_add(element_instructions, fixed, "bytecode instructions")?;
                if let Some(HirObjectPropertyKey::Computed(key)) = key {
                    element_instructions = checked_count_add(
                        element_instructions,
                        checked_count_add(
                            expression_instruction_count(key)?,
                            2,
                            "bytecode instructions",
                        )?,
                        "bytecode instructions",
                    )?;
                }
            }
            let class_create_instructions = if class.super_class.is_some() { 4 } else { 1 };
            let fixed = checked_count_add(
                class_create_instructions
                    + usize::from(class.name.is_some())
                    + usize::from(class.name_binding.is_some())
                    + usize::from(class.name_binding.is_some() || !class.private_names.is_empty())
                        * 2
                    + class.private_names.len() * 2,
                usize::from(class.elements.iter().any(|element| match element {
                    crate::HirClassElement::Method(method) => !method.is_static,
                    crate::HirClassElement::PublicField(field) => !field.is_static,
                    crate::HirClassElement::PrivateField(field) => !field.is_static,
                    crate::HirClassElement::PrivateMethod(method) => !method.is_static,
                    crate::HirClassElement::PrivateAccessor(accessor) => !accessor.is_static,
                    crate::HirClassElement::StaticBlock(_) => false,
                })),
                "bytecode instructions",
            )?;
            let fixed = checked_count_add(fixed, element_instructions, "bytecode instructions")?;
            checked_count_add(
                class
                    .super_class
                    .as_deref()
                    .map(expression_instruction_count)
                    .transpose()?
                    .unwrap_or(0),
                fixed,
                "bytecode instructions",
            )
        }
        HirExpressionKind::SuperStaticMember(_) => Ok(1),
        HirExpressionKind::SuperComputedMember(property) => checked_count_add(
            expression_instruction_count(property)?,
            3,
            "bytecode instructions",
        ),
        HirExpressionKind::Yield { argument, delegate } => {
            let argument = argument
                .as_deref()
                .map(expression_instruction_count)
                .transpose()?
                .unwrap_or(0);
            checked_count_add(
                argument,
                if *delegate { 53 } else { 1 },
                "bytecode instructions",
            )
        }
        HirExpressionKind::Await(argument) => checked_count_add(
            expression_instruction_count(argument)?,
            1,
            "bytecode instructions",
        ),
        HirExpressionKind::DynamicImport { source, options } => {
            let mut count = expression_instruction_count(source)?;
            count = checked_count_add(
                count,
                options
                    .as_deref()
                    .map(expression_instruction_count)
                    .transpose()?
                    .unwrap_or(1),
                "bytecode instructions",
            )?;
            checked_count_add(count, 1, "bytecode instructions")
        }
        HirExpressionKind::Sequence(expressions) => {
            let mut count = 0;
            for expression in expressions.iter() {
                count = checked_count_add(
                    count,
                    expression_instruction_count(expression)?,
                    "bytecode instructions",
                )?;
            }
            Ok(count)
        }
        HirExpressionKind::Object(properties) | HirExpressionKind::Array(properties) => {
            let mut count = 1;
            for property in properties.iter() {
                count = checked_count_add(
                    count,
                    object_property_instruction_count(&property.value)?,
                    "bytecode instructions",
                )?;
                if let HirObjectPropertyKey::Computed(key) = &property.key {
                    count = checked_count_add(
                        count,
                        expression_instruction_count(key)?,
                        "bytecode instructions",
                    )?;
                    count = checked_count_add(count, 1, "bytecode instructions")?;
                }
                count = checked_count_add(count, 1, "bytecode instructions")?;
            }
            Ok(count)
        }
        HirExpressionKind::ObjectSpread(parts) => {
            let mut count = 3;
            for part in parts.iter() {
                let nested = match part {
                    crate::hir::HirObjectExpressionPart::Property(property) => {
                        let mut count = object_property_instruction_count(&property.value)?;
                        if let HirObjectPropertyKey::Computed(key) = &property.key {
                            count = checked_count_add(
                                count,
                                expression_instruction_count(key)?,
                                "bytecode instructions",
                            )?;
                            count = checked_count_add(count, 1, "bytecode instructions")?;
                        }
                        checked_count_add(count, 1, "bytecode instructions")?
                    }
                    crate::hir::HirObjectExpressionPart::Spread(source) => checked_count_add(
                        expression_instruction_count(source)?,
                        1,
                        "bytecode instructions",
                    )?,
                };
                count = checked_count_add(count, nested, "bytecode instructions")?;
            }
            Ok(count)
        }
        HirExpressionKind::Binary {
            operator,
            left,
            right,
        } => {
            let operands = checked_count_add(
                expression_instruction_count(left)?,
                expression_instruction_count(right)?,
                "bytecode instructions",
            )?;
            let own = 1 + usize::from(matches!(
                operator,
                HirBinaryOperator::NotEqual
                    | HirBinaryOperator::StrictNotEqual
                    | HirBinaryOperator::In
            ));
            checked_count_add(own, operands, "bytecode instructions")
        }
        HirExpressionKind::Logical { left, right, .. } => {
            let operands = checked_count_add(
                expression_instruction_count(left)?,
                expression_instruction_count(right)?,
                "bytecode instructions",
            )?;
            checked_count_add(operands, 3, "bytecode instructions")
        }
        HirExpressionKind::StaticMember { object, .. } => checked_count_add(
            expression_instruction_count(object)?,
            1,
            "bytecode instructions",
        ),
        HirExpressionKind::ComputedMember { object, property } => {
            let operands = checked_count_add(
                expression_instruction_count(object)?,
                expression_instruction_count(property)?,
                "bytecode instructions",
            )?;
            checked_count_add(operands, 2, "bytecode instructions")
        }
        HirExpressionKind::PrivateMember { object, .. } => checked_count_add(
            expression_instruction_count(object)?,
            2,
            "bytecode instructions",
        ),
        HirExpressionKind::PrivateIn { object, .. } => checked_count_add(
            expression_instruction_count(object)?,
            2,
            "bytecode instructions",
        ),
        HirExpressionKind::Assignment {
            operator,
            target,
            value,
        } => {
            let Some(target) = target.assignment_target() else {
                return expression_instruction_count(value);
            };
            let computed_target = matches!(target, HirAssignmentTarget::ComputedMember { .. });
            let private_target = matches!(target, HirAssignmentTarget::PrivateMember { .. });
            let target = match target {
                HirAssignmentTarget::Identifier(_) => 0,
                HirAssignmentTarget::StaticMember { object, .. } => {
                    expression_instruction_count(object)?
                }
                HirAssignmentTarget::ComputedMember { object, property } => checked_count_add(
                    expression_instruction_count(object)?,
                    expression_instruction_count(property)?,
                    "bytecode instructions",
                )?,
                HirAssignmentTarget::PrivateMember { object, .. } => {
                    expression_instruction_count(object)?
                }
            };
            let operands = checked_count_add(
                target,
                expression_instruction_count(value)?,
                "bytecode instructions",
            )?;
            let own_instructions = match (operator, target) {
                (HirAssignmentOperator::Assign, _) => 1,
                (HirAssignmentOperator::Binary(_), _) => 3,
                (HirAssignmentOperator::Logical(_), _) => 5,
            };
            checked_count_add(
                operands,
                own_instructions + usize::from(computed_target) + usize::from(private_target),
                "bytecode instructions",
            )
        }
        HirExpressionKind::Update { prefix, target, .. } => {
            let identifier_target = matches!(target, HirAssignmentTarget::Identifier(_));
            let target = match target {
                HirAssignmentTarget::Identifier(_) => 0,
                HirAssignmentTarget::StaticMember { object, .. } => checked_count_add(
                    expression_instruction_count(object)?,
                    1,
                    "bytecode instructions",
                )?,
                HirAssignmentTarget::ComputedMember { object, property } => {
                    let operands = checked_count_add(
                        expression_instruction_count(object)?,
                        expression_instruction_count(property)?,
                        "bytecode instructions",
                    )?;
                    checked_count_add(operands, 2, "bytecode instructions")?
                }
                HirAssignmentTarget::PrivateMember { object, .. } => checked_count_add(
                    expression_instruction_count(object)?,
                    2,
                    "bytecode instructions",
                )?,
            };
            let own = match (identifier_target, *prefix) {
                (true, true) => 4,
                (true, false) => 5,
                (false, true) => 3,
                (false, false) => 4,
            };
            checked_count_add(target, own, "bytecode instructions")
        }
        HirExpressionKind::Unary { argument, .. } => checked_count_add(
            expression_instruction_count(argument)?,
            1,
            "bytecode instructions",
        ),
        HirExpressionKind::Conditional {
            test,
            consequent,
            alternate,
        } => {
            let arms = checked_count_add(
                expression_instruction_count(consequent)?,
                expression_instruction_count(alternate)?,
                "bytecode instructions",
            )?;
            let branches = checked_count_add(arms, 4, "bytecode instructions")?;
            checked_count_add(
                expression_instruction_count(test)?,
                branches,
                "bytecode instructions",
            )
        }
        HirExpressionKind::Call { callee, arguments }
        | HirExpressionKind::New { callee, arguments } => {
            let mut count = expression_instruction_count(callee)?;
            count = checked_count_add(
                count,
                if matches!(
                    callee.kind,
                    HirExpressionKind::SuperStaticMember(_)
                        | HirExpressionKind::SuperComputedMember(_)
                ) {
                    3
                } else {
                    2
                },
                "bytecode instructions",
            )?;
            for argument in arguments.iter() {
                count = checked_count_add(
                    count,
                    expression_instruction_count(argument)?,
                    "bytecode instructions",
                )?;
                count = checked_count_add(count, 1, "bytecode instructions")?;
            }
            Ok(count)
        }
        HirExpressionKind::TaggedTemplate {
            tag, substitutions, ..
        } => {
            let mut count = match &tag.kind {
                HirExpressionKind::StaticMember { object, .. } => checked_count_add(
                    expression_instruction_count(object)?,
                    2,
                    "bytecode instructions",
                )?,
                HirExpressionKind::ComputedMember { object, property } => {
                    let nested = checked_count_add(
                        expression_instruction_count(object)?,
                        expression_instruction_count(property)?,
                        "bytecode instructions",
                    )?;
                    checked_count_add(nested, 3, "bytecode instructions")?
                }
                HirExpressionKind::PrivateMember { object, .. } => checked_count_add(
                    expression_instruction_count(object)?,
                    3,
                    "bytecode instructions",
                )?,
                HirExpressionKind::SuperStaticMember(_) => 2,
                HirExpressionKind::SuperComputedMember(property) => checked_count_add(
                    expression_instruction_count(property)?,
                    5,
                    "bytecode instructions",
                )?,
                _ => checked_count_add(
                    expression_instruction_count(tag)?,
                    1,
                    "bytecode instructions",
                )?,
            };
            count = checked_count_add(count, 2, "bytecode instructions")?;
            for substitution in substitutions.iter() {
                count = checked_count_add(
                    count,
                    expression_instruction_count(substitution)?,
                    "bytecode instructions",
                )?;
                count = checked_count_add(count, 1, "bytecode instructions")?;
            }
            Ok(count)
        }
        HirExpressionKind::OptionalChain { base, links } => {
            let mut count = checked_count_add(
                expression_instruction_count(base)?,
                6,
                "bytecode instructions",
            )?;
            for link in links.iter() {
                if link.optional {
                    count = checked_count_add(count, 2, "bytecode instructions")?;
                }
                match &link.kind {
                    crate::HirOptionalChainLinkKind::StaticMember(_) => {
                        count = checked_count_add(count, 2, "bytecode instructions")?;
                    }
                    crate::HirOptionalChainLinkKind::ComputedMember(property) => {
                        count = checked_count_add(
                            count,
                            expression_instruction_count(property)?,
                            "bytecode instructions",
                        )?;
                        count = checked_count_add(count, 3, "bytecode instructions")?;
                    }
                    crate::HirOptionalChainLinkKind::PrivateMember(_) => {
                        count = checked_count_add(count, 3, "bytecode instructions")?;
                    }
                    crate::HirOptionalChainLinkKind::Call(arguments) => {
                        count = checked_count_add(count, 5, "bytecode instructions")?;
                        for argument in arguments.iter() {
                            let (expression, own) = match argument {
                                crate::hir::HirArrayExpressionPart::Element(expression) => {
                                    (expression, 1)
                                }
                                crate::hir::HirArrayExpressionPart::Spread(expression) => {
                                    (expression, 16)
                                }
                                crate::hir::HirArrayExpressionPart::Elision => continue,
                            };
                            count = checked_count_add(
                                count,
                                expression_instruction_count(expression)?,
                                "bytecode instructions",
                            )?;
                            count = checked_count_add(count, own, "bytecode instructions")?;
                        }
                    }
                }
            }
            Ok(count)
        }
        HirExpressionKind::CallSpread { callee, arguments }
        | HirExpressionKind::NewSpread { callee, arguments } => {
            let mut count = checked_count_add(
                expression_instruction_count(callee)?,
                4,
                "bytecode instructions",
            )?;
            for argument in arguments.iter() {
                let (expression, own) = match argument {
                    crate::hir::HirArrayExpressionPart::Element(expression) => (expression, 3),
                    crate::hir::HirArrayExpressionPart::Spread(expression) => (expression, 16),
                    crate::hir::HirArrayExpressionPart::Elision => continue,
                };
                count = checked_count_add(
                    count,
                    expression_instruction_count(expression)?,
                    "bytecode instructions",
                )?;
                count = checked_count_add(count, own, "bytecode instructions")?;
            }
            Ok(count)
        }
        HirExpressionKind::SuperCall(arguments) => {
            let mut count = 2;
            for argument in arguments.iter() {
                count = checked_count_add(
                    count,
                    expression_instruction_count(argument)?,
                    "bytecode instructions",
                )?;
                count = checked_count_add(count, 1, "bytecode instructions")?;
            }
            Ok(count)
        }
        HirExpressionKind::SuperCallSpread(arguments) => {
            let mut count = 3;
            for argument in arguments.iter() {
                let (expression, own) = match argument {
                    crate::hir::HirArrayExpressionPart::Element(expression) => (expression, 3),
                    crate::hir::HirArrayExpressionPart::Spread(expression) => (expression, 16),
                    crate::hir::HirArrayExpressionPart::Elision => continue,
                };
                count = checked_count_add(
                    count,
                    expression_instruction_count(expression)?,
                    "bytecode instructions",
                )?;
                count = checked_count_add(count, own, "bytecode instructions")?;
            }
            Ok(count)
        }
        _ => Ok(1),
    }
}

fn object_property_instruction_count(
    value: &crate::HirObjectPropertyValue,
) -> Result<usize, CompileError> {
    match value {
        crate::HirObjectPropertyValue::Data(expression)
        | crate::HirObjectPropertyValue::Prototype(expression) => {
            expression_instruction_count(expression)
        }
        crate::HirObjectPropertyValue::Method(_)
        | crate::HirObjectPropertyValue::Getter(_)
        | crate::HirObjectPropertyValue::Setter(_) => Ok(3),
    }
}

/// Counts the prologue and expression instructions needed by default parameter initializers.
pub(super) fn parameter_initializer_instruction_count(
    initializers: &[Option<HirExpression>],
) -> Result<usize, CompileError> {
    let mut count = 0;
    for initializer in initializers.iter().flatten() {
        count = checked_count_add(
            count,
            expression_instruction_count(initializer)?,
            "function parameter instructions",
        )?;
        count = checked_count_add(count, 4, "function parameter instructions")?;
    }
    Ok(count)
}
