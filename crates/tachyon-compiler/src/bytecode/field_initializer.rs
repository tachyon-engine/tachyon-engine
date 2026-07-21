//! Capture discovery for synthetic class-field initializer stencils.

use crate::hir::HirObjectExpressionPart;
use crate::{
    BindingId, HirAssignmentTarget, HirClassElement, HirExpression, HirExpressionKind,
    HirFunctionKind, HirObjectPropertyKey, HirObjectPropertyValue, HirPattern, HirPatternKind,
    HirProgram, HirStatementKind,
};

/// Returns bindings referenced by field initializers that Oxc did not model as function scopes.
pub(super) fn forced_captures(hir: &HirProgram) -> Vec<BindingId> {
    let mut bindings = Vec::new();
    for function in hir.functions() {
        if function.kind != HirFunctionKind::ClassFieldInitializer {
            continue;
        }
        for statement in function.body.iter() {
            if let HirStatementKind::Return(Some(expression)) = &statement.kind {
                collect_expression(expression, &mut bindings);
            }
        }
    }
    bindings
}

fn record(binding: Option<BindingId>, bindings: &mut Vec<BindingId>) {
    if let Some(binding) = binding
        && !bindings.contains(&binding)
    {
        bindings.push(binding);
    }
}

/// Traverses one initializer without entering separately-owned nested function stencils.
fn collect_expression(expression: &HirExpression, bindings: &mut Vec<BindingId>) {
    match &expression.kind {
        HirExpressionKind::Identifier(reference) => record(reference.binding, bindings),
        HirExpressionKind::Sequence(expressions) => {
            for expression in expressions.iter() {
                collect_expression(expression, bindings);
            }
        }
        HirExpressionKind::Object(properties) | HirExpressionKind::Array(properties) => {
            for property in properties.iter() {
                if let HirObjectPropertyKey::Computed(key) = &property.key {
                    collect_expression(key, bindings);
                }
                if let HirObjectPropertyValue::Data(value) = &property.value {
                    collect_expression(value, bindings);
                }
            }
        }
        HirExpressionKind::ObjectSpread(parts) => {
            for part in parts.iter() {
                match part {
                    HirObjectExpressionPart::Property(property) => {
                        if let HirObjectPropertyKey::Computed(key) = &property.key {
                            collect_expression(key, bindings);
                        }
                        if let HirObjectPropertyValue::Data(value) = &property.value {
                            collect_expression(value, bindings);
                        }
                    }
                    HirObjectExpressionPart::Spread(expression) => {
                        collect_expression(expression, bindings)
                    }
                }
            }
        }
        HirExpressionKind::StaticMember { object, .. } => collect_expression(object, bindings),
        HirExpressionKind::ComputedMember { object, property } => {
            collect_expression(object, bindings);
            collect_expression(property, bindings);
        }
        HirExpressionKind::SuperComputedMember(property)
        | HirExpressionKind::Unary {
            argument: property, ..
        } => collect_expression(property, bindings),
        HirExpressionKind::Binary { left, right, .. }
        | HirExpressionKind::Logical { left, right, .. } => {
            collect_expression(left, bindings);
            collect_expression(right, bindings);
        }
        HirExpressionKind::Assignment { target, value, .. } => {
            collect_target(target, bindings);
            collect_expression(value, bindings);
        }
        HirExpressionKind::Update { target, .. } => collect_assignment_target(target, bindings),
        HirExpressionKind::Conditional {
            test,
            consequent,
            alternate,
        } => {
            collect_expression(test, bindings);
            collect_expression(consequent, bindings);
            collect_expression(alternate, bindings);
        }
        HirExpressionKind::Call { callee, arguments }
        | HirExpressionKind::New { callee, arguments } => {
            collect_expression(callee, bindings);
            for argument in arguments.iter() {
                collect_expression(argument, bindings);
            }
        }
        HirExpressionKind::SuperCall(arguments) => {
            for argument in arguments.iter() {
                collect_expression(argument, bindings);
            }
        }
        HirExpressionKind::Class(class) => {
            if let Some(super_class) = &class.super_class {
                collect_expression(super_class, bindings);
            }
            for element in class.elements.iter() {
                let key = match element {
                    HirClassElement::Method(method) => &method.key,
                    HirClassElement::PublicField(field) => &field.key,
                };
                if let HirObjectPropertyKey::Computed(key) = key {
                    collect_expression(key, bindings);
                }
            }
        }
        HirExpressionKind::Number(_)
        | HirExpressionKind::String(_)
        | HirExpressionKind::RegExp { .. }
        | HirExpressionKind::Boolean(_)
        | HirExpressionKind::Null
        | HirExpressionKind::Function(_)
        | HirExpressionKind::This
        | HirExpressionKind::NewTarget
        | HirExpressionKind::SuperStaticMember(_) => {}
    }
}

/// Traverses assignment-pattern expressions retained by an initializer.
fn collect_target(target: &HirPattern, bindings: &mut Vec<BindingId>) {
    match &target.kind {
        HirPatternKind::Assignment(target) => collect_assignment_target(target, bindings),
        HirPatternKind::Default {
            target,
            initializer,
        } => {
            collect_target(target, bindings);
            collect_expression(initializer, bindings);
        }
        HirPatternKind::Array { elements, rest } => {
            for element in elements.iter().flatten() {
                collect_target(element, bindings);
            }
            if let Some(rest) = rest {
                collect_target(rest, bindings);
            }
        }
        HirPatternKind::Object { properties, rest } => {
            for property in properties.iter() {
                if let HirObjectPropertyKey::Computed(key) = &property.key {
                    collect_expression(key, bindings);
                }
                collect_target(&property.target, bindings);
            }
            if let Some(rest) = rest {
                collect_target(rest, bindings);
            }
        }
        HirPatternKind::Binding(_) => {}
    }
}

fn collect_assignment_target(target: &HirAssignmentTarget, bindings: &mut Vec<BindingId>) {
    match target {
        HirAssignmentTarget::Identifier(reference) => record(reference.binding, bindings),
        HirAssignmentTarget::StaticMember { object, .. } => collect_expression(object, bindings),
        HirAssignmentTarget::ComputedMember { object, property } => {
            collect_expression(object, bindings);
            collect_expression(property, bindings);
        }
    }
}
