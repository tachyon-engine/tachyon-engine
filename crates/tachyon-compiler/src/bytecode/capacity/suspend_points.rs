use super::checked_count_add;
use crate::hir::{HirArrayExpressionPart, HirForInLeft, HirObjectExpressionPart};
use crate::{
    CompileError, HirAssignmentTarget, HirExpression, HirExpressionKind, HirForInitializer,
    HirObjectPropertyKey, HirObjectPropertyValue, HirPattern, HirPatternKind, HirStatement,
    HirStatementKind, HirVariableDeclaration,
};

const COLLECTION: &str = "suspend points";

/// Counts every yield owned by one function body without crossing nested function stencils.
pub(super) fn function_suspend_point_count(
    statements: &[HirStatement],
    parameter_initializers: &[Option<HirExpression>],
) -> Result<usize, CompileError> {
    let mut count = statements_suspend_point_count(statements)?;
    for initializer in parameter_initializers.iter().flatten() {
        count = add(count, expression_suspend_point_count(initializer)?)?;
    }
    Ok(count)
}

/// Walks structured statements and all expression-bearing control-flow children.
fn statements_suspend_point_count(statements: &[HirStatement]) -> Result<usize, CompileError> {
    let mut count = 0;
    for statement in statements {
        let nested = match &statement.kind {
            HirStatementKind::Expression(expression) => expression_suspend_point_count(expression)?,
            HirStatementKind::VariableDeclaration(declaration) => {
                declaration_suspend_point_count(declaration)?
            }
            HirStatementKind::Block(body) => statements_suspend_point_count(body)?,
            HirStatementKind::If {
                test,
                consequent,
                alternate,
            } => {
                let mut nested = expression_suspend_point_count(test)?;
                nested = add(nested, statement_suspend_point_count(consequent)?)?;
                if let Some(alternate) = alternate {
                    nested = add(nested, statement_suspend_point_count(alternate)?)?;
                }
                nested
            }
            HirStatementKind::For {
                initializer,
                test,
                update,
                body,
            } => {
                let mut nested = match initializer {
                    Some(HirForInitializer::Variable(declaration)) => {
                        declaration_suspend_point_count(declaration)?
                    }
                    Some(HirForInitializer::Expression(expression)) => {
                        expression_suspend_point_count(expression)?
                    }
                    None => 0,
                };
                for expression in [test.as_ref(), update.as_ref()].into_iter().flatten() {
                    nested = add(nested, expression_suspend_point_count(expression)?)?;
                }
                add(nested, statement_suspend_point_count(body)?)?
            }
            HirStatementKind::ForIn { left, right, body } => {
                let nested = add(
                    for_in_left_suspend_point_count(left)?,
                    expression_suspend_point_count(right)?,
                )?;
                add(nested, statement_suspend_point_count(body)?)?
            }
            HirStatementKind::ForOf {
                r#await,
                left,
                right,
                body,
            } => {
                let nested = add(
                    for_in_left_suspend_point_count(left)?,
                    expression_suspend_point_count(right)?,
                )?;
                let nested = add(nested, statement_suspend_point_count(body)?)?;
                add(nested, usize::from(*r#await) * 2)?
            }
            HirStatementKind::Loop { test, body, .. } => add(
                expression_suspend_point_count(test)?,
                statement_suspend_point_count(body)?,
            )?,
            HirStatementKind::Switch {
                discriminant,
                cases,
            } => {
                let mut nested = expression_suspend_point_count(discriminant)?;
                for case in cases.iter() {
                    if let Some(test) = &case.test {
                        nested = add(nested, expression_suspend_point_count(test)?)?;
                    }
                    nested = add(nested, statements_suspend_point_count(&case.consequent)?)?;
                }
                nested
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                let mut nested = statements_suspend_point_count(block)?;
                if let Some(handler) = handler {
                    if let Some(parameter) = &handler.parameter {
                        nested = add(nested, pattern_suspend_point_count(parameter)?)?;
                    }
                    nested = add(nested, statements_suspend_point_count(&handler.body)?)?;
                }
                if let Some(finalizer) = finalizer {
                    nested = add(nested, statements_suspend_point_count(finalizer)?)?;
                }
                nested
            }
            HirStatementKind::Return(argument) => argument
                .as_ref()
                .map(expression_suspend_point_count)
                .transpose()?
                .unwrap_or(0),
            HirStatementKind::Throw(argument) => expression_suspend_point_count(argument)?,
            HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Empty => 0,
        };
        count = add(count, nested)?;
    }
    Ok(count)
}

fn statement_suspend_point_count(statement: &HirStatement) -> Result<usize, CompileError> {
    statements_suspend_point_count(core::slice::from_ref(statement))
}

fn declaration_suspend_point_count(
    declaration: &HirVariableDeclaration,
) -> Result<usize, CompileError> {
    let mut count = 0;
    for declarator in declaration.declarators.iter() {
        count = add(count, pattern_suspend_point_count(&declarator.pattern)?)?;
        if let Some(initializer) = &declarator.initializer {
            count = add(count, expression_suspend_point_count(initializer)?)?;
        }
    }
    Ok(count)
}

fn for_in_left_suspend_point_count(left: &HirForInLeft) -> Result<usize, CompileError> {
    match left {
        HirForInLeft::Variable(declaration) => declaration_suspend_point_count(declaration),
        HirForInLeft::Assignment(pattern) => pattern_suspend_point_count(pattern),
    }
}

/// Walks patterns because computed keys and defaults may suspend inside a generator.
fn pattern_suspend_point_count(pattern: &HirPattern) -> Result<usize, CompileError> {
    match &pattern.kind {
        HirPatternKind::Binding(_) => Ok(0),
        HirPatternKind::Assignment(target) => assignment_target_suspend_point_count(target),
        HirPatternKind::Default {
            target,
            initializer,
        } => add(
            pattern_suspend_point_count(target)?,
            expression_suspend_point_count(initializer)?,
        ),
        HirPatternKind::Array { elements, rest } => {
            let mut count = 0;
            for element in elements.iter().flatten() {
                count = add(count, pattern_suspend_point_count(element)?)?;
            }
            if let Some(rest) = rest {
                count = add(count, pattern_suspend_point_count(rest)?)?;
            }
            Ok(count)
        }
        HirPatternKind::Object { properties, rest } => {
            let mut count = 0;
            for property in properties.iter() {
                if let HirObjectPropertyKey::Computed(key) = &property.key {
                    count = add(count, expression_suspend_point_count(key)?)?;
                }
                count = add(count, pattern_suspend_point_count(&property.target)?)?;
            }
            if let Some(rest) = rest {
                count = add(count, pattern_suspend_point_count(rest)?)?;
            }
            Ok(count)
        }
    }
}

/// Counts one expression and every same-function child in evaluation order.
fn expression_suspend_point_count(expression: &HirExpression) -> Result<usize, CompileError> {
    match &expression.kind {
        HirExpressionKind::Yield { argument, .. } => add(
            1,
            argument
                .as_deref()
                .map(expression_suspend_point_count)
                .transpose()?
                .unwrap_or(0),
        ),
        HirExpressionKind::Await(argument) => add(1, expression_suspend_point_count(argument)?),
        HirExpressionKind::DynamicImport { source, options } => add(
            expression_suspend_point_count(source)?,
            options
                .as_deref()
                .map(expression_suspend_point_count)
                .transpose()?
                .unwrap_or(0),
        ),
        HirExpressionKind::Class(class) => {
            let mut count = class
                .super_class
                .as_deref()
                .map(expression_suspend_point_count)
                .transpose()?
                .unwrap_or(0);
            for element in class.elements.iter() {
                let key = match element {
                    crate::HirClassElement::Method(method) => Some(&method.key),
                    crate::HirClassElement::PublicField(field) => Some(&field.key),
                    crate::HirClassElement::PrivateField(_)
                    | crate::HirClassElement::PrivateMethod(_)
                    | crate::HirClassElement::PrivateAccessor(_)
                    | crate::HirClassElement::StaticBlock(_) => None,
                };
                if let Some(HirObjectPropertyKey::Computed(key)) = key {
                    count = add(count, expression_suspend_point_count(key)?)?;
                }
            }
            Ok(count)
        }
        HirExpressionKind::Sequence(expressions) => expression_list_count(expressions),
        HirExpressionKind::Object(properties) | HirExpressionKind::Array(properties) => {
            let mut count = 0;
            for property in properties.iter() {
                if let HirObjectPropertyKey::Computed(key) = &property.key {
                    count = add(count, expression_suspend_point_count(key)?)?;
                }
                if let HirObjectPropertyValue::Data(value) = &property.value {
                    count = add(count, expression_suspend_point_count(value)?)?;
                }
            }
            Ok(count)
        }
        HirExpressionKind::ObjectSpread(parts) => {
            let mut count = 0;
            for part in parts.iter() {
                match part {
                    HirObjectExpressionPart::Property(property) => {
                        if let HirObjectPropertyKey::Computed(key) = &property.key {
                            count = add(count, expression_suspend_point_count(key)?)?;
                        }
                        if let HirObjectPropertyValue::Data(value) = &property.value {
                            count = add(count, expression_suspend_point_count(value)?)?;
                        }
                    }
                    HirObjectExpressionPart::Spread(source) => {
                        count = add(count, expression_suspend_point_count(source)?)?;
                    }
                }
            }
            Ok(count)
        }
        HirExpressionKind::ArrayAccumulation(parts) => {
            let mut count = 0;
            for part in parts.iter() {
                match part {
                    HirArrayExpressionPart::Element(expression)
                    | HirArrayExpressionPart::Spread(expression) => {
                        count = add(count, expression_suspend_point_count(expression)?)?;
                    }
                    HirArrayExpressionPart::Elision => {}
                }
            }
            Ok(count)
        }
        HirExpressionKind::StaticMember { object, .. }
        | HirExpressionKind::PrivateMember { object, .. }
        | HirExpressionKind::PrivateIn { object, .. }
        | HirExpressionKind::Unary {
            argument: object, ..
        } => expression_suspend_point_count(object),
        HirExpressionKind::SuperComputedMember(property) => {
            expression_suspend_point_count(property)
        }
        HirExpressionKind::ComputedMember { object, property }
        | HirExpressionKind::Binary {
            left: object,
            right: property,
            ..
        }
        | HirExpressionKind::Logical {
            left: object,
            right: property,
            ..
        } => add(
            expression_suspend_point_count(object)?,
            expression_suspend_point_count(property)?,
        ),
        HirExpressionKind::Assignment { target, value, .. } => add(
            pattern_suspend_point_count(target)?,
            expression_suspend_point_count(value)?,
        ),
        HirExpressionKind::Update { target, .. } => assignment_target_suspend_point_count(target),
        HirExpressionKind::Conditional {
            test,
            consequent,
            alternate,
        } => add(
            expression_suspend_point_count(test)?,
            add(
                expression_suspend_point_count(consequent)?,
                expression_suspend_point_count(alternate)?,
            )?,
        ),
        HirExpressionKind::Call { callee, arguments }
        | HirExpressionKind::New { callee, arguments } => add(
            expression_suspend_point_count(callee)?,
            expression_list_count(arguments)?,
        ),
        HirExpressionKind::CallSpread { callee, arguments } => {
            let mut count = expression_suspend_point_count(callee)?;
            for argument in arguments.iter() {
                let expression = match argument {
                    HirArrayExpressionPart::Element(expression)
                    | HirArrayExpressionPart::Spread(expression) => expression,
                    HirArrayExpressionPart::Elision => continue,
                };
                count = add(count, expression_suspend_point_count(expression)?)?;
            }
            Ok(count)
        }
        HirExpressionKind::SuperCall(arguments) => expression_list_count(arguments),
        HirExpressionKind::Number(_)
        | HirExpressionKind::BigInt(_)
        | HirExpressionKind::String(_)
        | HirExpressionKind::RegExp { .. }
        | HirExpressionKind::Boolean(_)
        | HirExpressionKind::Null
        | HirExpressionKind::Identifier(_)
        | HirExpressionKind::Function(_)
        | HirExpressionKind::This
        | HirExpressionKind::NewTarget
        | HirExpressionKind::SuperStaticMember(_) => Ok(0),
    }
}

fn expression_list_count(expressions: &[HirExpression]) -> Result<usize, CompileError> {
    let mut count = 0;
    for expression in expressions {
        count = add(count, expression_suspend_point_count(expression)?)?;
    }
    Ok(count)
}

fn assignment_target_suspend_point_count(
    target: &HirAssignmentTarget,
) -> Result<usize, CompileError> {
    match target {
        HirAssignmentTarget::Identifier(_) => Ok(0),
        HirAssignmentTarget::StaticMember { object, .. }
        | HirAssignmentTarget::PrivateMember { object, .. } => {
            expression_suspend_point_count(object)
        }
        HirAssignmentTarget::ComputedMember { object, property } => add(
            expression_suspend_point_count(object)?,
            expression_suspend_point_count(property)?,
        ),
    }
}

#[inline]
fn add(total: usize, nested: usize) -> Result<usize, CompileError> {
    checked_count_add(total, nested, COLLECTION)
}
