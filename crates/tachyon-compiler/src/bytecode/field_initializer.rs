//! Capture discovery for synthetic class-field initializer stencils.

use crate::hir::{HirArrayExpressionPart, HirForInLeft, HirObjectExpressionPart};
use crate::{
    BindingId, HirAssignmentTarget, HirClassElement, HirExpression, HirExpressionKind,
    HirForInitializer, HirFunctionKind, HirFunctionRole, HirObjectPropertyKey,
    HirObjectPropertyValue, HirPattern, HirPatternKind, HirProgram, HirStatement, HirStatementKind,
    HirVariableDeclaration,
};

/// Returns bindings crossing synthetic class-initializer boundaries absent from Oxc's function tree.
pub(super) fn forced_captures(hir: &HirProgram) -> Vec<BindingId> {
    let mut bindings = Vec::new();
    for function in hir.functions() {
        match (function.kind, function.role) {
            (HirFunctionKind::Generator | HirFunctionKind::AsyncGenerator, _) => {}
            (_, HirFunctionRole::ClassFieldInitializer) => {
                collect_statements(&function.body, &mut bindings);
            }
            _ => {}
        }
    }
    bindings
}

/// Traverses a static block without entering separately-owned nested function stencils.
fn collect_statements(statements: &[HirStatement], bindings: &mut Vec<BindingId>) {
    for statement in statements {
        match &statement.kind {
            HirStatementKind::Expression(expression) | HirStatementKind::Throw(expression) => {
                collect_expression(expression, bindings);
            }
            HirStatementKind::Return(expression) => {
                if let Some(expression) = expression {
                    collect_expression(expression, bindings);
                }
            }
            HirStatementKind::VariableDeclaration(declaration) => {
                collect_declaration(declaration, bindings);
            }
            HirStatementKind::FunctionDeclaration(_)
            | HirStatementKind::Break
            | HirStatementKind::Continue
            | HirStatementKind::Empty => {}
            HirStatementKind::Block(body) => collect_statements(body, bindings),
            HirStatementKind::If {
                test,
                consequent,
                alternate,
            } => {
                collect_expression(test, bindings);
                collect_statements(core::slice::from_ref(consequent), bindings);
                if let Some(alternate) = alternate {
                    collect_statements(core::slice::from_ref(alternate), bindings);
                }
            }
            HirStatementKind::For {
                initializer,
                test,
                update,
                body,
            } => {
                if let Some(initializer) = initializer {
                    match initializer {
                        HirForInitializer::Variable(declaration) => {
                            collect_declaration(declaration, bindings);
                        }
                        HirForInitializer::Expression(expression) => {
                            collect_expression(expression, bindings);
                        }
                    }
                }
                if let Some(test) = test {
                    collect_expression(test, bindings);
                }
                if let Some(update) = update {
                    collect_expression(update, bindings);
                }
                collect_statements(core::slice::from_ref(body), bindings);
            }
            HirStatementKind::ForIn { left, right, body }
            | HirStatementKind::ForOf {
                left, right, body, ..
            } => {
                collect_for_in_left(left, bindings);
                collect_expression(right, bindings);
                collect_statements(core::slice::from_ref(body), bindings);
            }
            HirStatementKind::Loop { test, body, .. } => {
                collect_expression(test, bindings);
                collect_statements(core::slice::from_ref(body), bindings);
            }
            HirStatementKind::Switch {
                discriminant,
                cases,
            } => {
                collect_expression(discriminant, bindings);
                for case in cases.iter() {
                    if let Some(test) = &case.test {
                        collect_expression(test, bindings);
                    }
                    collect_statements(&case.consequent, bindings);
                }
            }
            HirStatementKind::Try {
                block,
                handler,
                finalizer,
            } => {
                collect_statements(block, bindings);
                if let Some(handler) = handler {
                    if let Some(parameter) = &handler.parameter {
                        collect_target(parameter, bindings);
                    }
                    collect_statements(&handler.body, bindings);
                }
                if let Some(finalizer) = finalizer {
                    collect_statements(finalizer, bindings);
                }
            }
        }
    }
}

fn collect_declaration(declaration: &HirVariableDeclaration, bindings: &mut Vec<BindingId>) {
    for declarator in declaration.declarators.iter() {
        collect_target(&declarator.pattern, bindings);
        if let Some(initializer) = &declarator.initializer {
            collect_expression(initializer, bindings);
        }
    }
}

fn collect_for_in_left(left: &HirForInLeft, bindings: &mut Vec<BindingId>) {
    match left {
        HirForInLeft::Variable(declaration) => collect_declaration(declaration, bindings),
        HirForInLeft::Assignment(pattern) => collect_target(pattern, bindings),
    }
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
        HirExpressionKind::ArrayAccumulation(parts) => {
            for part in parts.iter() {
                match part {
                    crate::hir::HirArrayExpressionPart::Element(expression)
                    | crate::hir::HirArrayExpressionPart::Spread(expression) => {
                        collect_expression(expression, bindings);
                    }
                    crate::hir::HirArrayExpressionPart::Elision => {}
                }
            }
        }
        HirExpressionKind::StaticMember { object, .. } => collect_expression(object, bindings),
        HirExpressionKind::ComputedMember { object, property } => {
            collect_expression(object, bindings);
            collect_expression(property, bindings);
        }
        HirExpressionKind::PrivateMember { object, .. } => collect_expression(object, bindings),
        HirExpressionKind::PrivateIn { object, .. } => collect_expression(object, bindings),
        HirExpressionKind::SuperComputedMember(property)
        | HirExpressionKind::Unary {
            argument: property, ..
        } => collect_expression(property, bindings),
        HirExpressionKind::Yield { argument, .. } => {
            if let Some(argument) = argument {
                collect_expression(argument, bindings);
            }
        }
        HirExpressionKind::Await(argument) => collect_expression(argument, bindings),
        HirExpressionKind::DynamicImport { source, options } => {
            collect_expression(source, bindings);
            if let Some(options) = options {
                collect_expression(options, bindings);
            }
        }
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
        HirExpressionKind::TaggedTemplate {
            tag, substitutions, ..
        } => {
            collect_expression(tag, bindings);
            for substitution in substitutions.iter() {
                collect_expression(substitution, bindings);
            }
        }
        HirExpressionKind::OptionalChain { base, links } => {
            collect_expression(base, bindings);
            for link in links.iter() {
                match &link.kind {
                    crate::HirOptionalChainLinkKind::ComputedMember(property) => {
                        collect_expression(property, bindings);
                    }
                    crate::HirOptionalChainLinkKind::Call(arguments) => {
                        for argument in arguments.iter() {
                            match argument {
                                HirArrayExpressionPart::Element(expression)
                                | HirArrayExpressionPart::Spread(expression) => {
                                    collect_expression(expression, bindings);
                                }
                                HirArrayExpressionPart::Elision => {}
                            }
                        }
                    }
                    crate::HirOptionalChainLinkKind::StaticMember(_)
                    | crate::HirOptionalChainLinkKind::PrivateMember(_) => {}
                }
            }
        }
        HirExpressionKind::CallSpread { callee, arguments } => {
            collect_expression(callee, bindings);
            for argument in arguments.iter() {
                match argument {
                    HirArrayExpressionPart::Element(expression)
                    | HirArrayExpressionPart::Spread(expression) => {
                        collect_expression(expression, bindings);
                    }
                    HirArrayExpressionPart::Elision => {}
                }
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
                    HirClassElement::Method(method) => Some(&method.key),
                    HirClassElement::PublicField(field) => Some(&field.key),
                    HirClassElement::PrivateField(_) => None,
                    HirClassElement::PrivateMethod(_) => None,
                    HirClassElement::PrivateAccessor(_) => None,
                    HirClassElement::StaticBlock(_) => None,
                };
                if let Some(HirObjectPropertyKey::Computed(key)) = key {
                    collect_expression(key, bindings);
                }
            }
        }
        HirExpressionKind::Number(_)
        | HirExpressionKind::BigInt(_)
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
        HirAssignmentTarget::PrivateMember { object, .. } => collect_expression(object, bindings),
    }
}
