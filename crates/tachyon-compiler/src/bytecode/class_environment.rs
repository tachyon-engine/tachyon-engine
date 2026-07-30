//! Discovery of named class-expression environments across owned HIR trees.

use crate::hir::HirPrivateName;
use crate::hir::{HirArrayExpressionPart, HirForInLeft, HirObjectExpressionPart};
use crate::{
    HirAssignmentTarget, HirBinding, HirExpression, HirExpressionKind, HirForInitializer,
    HirObjectPropertyKey, HirObjectPropertyValue, HirPattern, HirPatternKind, HirProgram,
    HirStatement, HirStatementKind, HirVariableDeclaration, ScopeId,
};

#[derive(Clone, Debug)]
pub(super) struct ClassEnvironment {
    pub(super) scope: ScopeId,
    pub(super) name_binding: Option<HirBinding>,
    pub(super) private_names: Box<[HirPrivateName]>,
}

/// Collects each class lexical environment once while keeping function stencils as traversal roots.
pub(super) fn collect(hir: &HirProgram) -> Vec<ClassEnvironment> {
    let capacity = hir.statements().len().saturating_add(hir.functions().len());
    let mut bindings = Vec::with_capacity(capacity);
    collect_statements(hir.statements(), &mut bindings);
    for function in hir.functions() {
        for parameter in function.parameters.iter() {
            collect_pattern(parameter, &mut bindings);
        }
        if let Some(rest) = &function.rest_parameter {
            collect_pattern(rest, &mut bindings);
        }
        for initializer in function.parameter_initializers.iter().flatten() {
            collect_expression(initializer, &mut bindings);
        }
        collect_statements(&function.body, &mut bindings);
    }
    bindings
}

/// Traverses statement-owned expressions without re-entering separately-owned function stencils.
fn collect_statements(statements: &[HirStatement], bindings: &mut Vec<ClassEnvironment>) {
    for statement in statements {
        match &statement.kind {
            HirStatementKind::Expression(expression) | HirStatementKind::Throw(expression) => {
                collect_expression(expression, bindings)
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
                        collect_pattern(parameter, bindings);
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

fn collect_declaration(declaration: &HirVariableDeclaration, bindings: &mut Vec<ClassEnvironment>) {
    for declarator in declaration.declarators.iter() {
        collect_pattern(&declarator.pattern, bindings);
        if let Some(initializer) = &declarator.initializer {
            collect_expression(initializer, bindings);
        }
    }
}

fn collect_for_in_left(left: &HirForInLeft, bindings: &mut Vec<ClassEnvironment>) {
    match left {
        HirForInLeft::Variable(declaration) => collect_declaration(declaration, bindings),
        HirForInLeft::Assignment(pattern) => collect_pattern(pattern, bindings),
    }
}

/// Traverses defaults and computed keys embedded in binding and assignment patterns.
fn collect_pattern(pattern: &HirPattern, bindings: &mut Vec<ClassEnvironment>) {
    match &pattern.kind {
        HirPatternKind::Binding(_) => {}
        HirPatternKind::Assignment(target) => collect_assignment_target(target, bindings),
        HirPatternKind::Default {
            target,
            initializer,
        } => {
            collect_pattern(target, bindings);
            collect_expression(initializer, bindings);
        }
        HirPatternKind::Array { elements, rest } => {
            for element in elements.iter().flatten() {
                collect_pattern(element, bindings);
            }
            if let Some(rest) = rest {
                collect_pattern(rest, bindings);
            }
        }
        HirPatternKind::Object { properties, rest } => {
            for property in properties.iter() {
                if let HirObjectPropertyKey::Computed(key) = &property.key {
                    collect_expression(key, bindings);
                }
                collect_pattern(&property.target, bindings);
            }
            if let Some(rest) = rest {
                collect_pattern(rest, bindings);
            }
        }
    }
}

fn collect_assignment_target(target: &HirAssignmentTarget, bindings: &mut Vec<ClassEnvironment>) {
    match target {
        HirAssignmentTarget::Identifier(_) => {}
        HirAssignmentTarget::StaticMember { object, .. } => collect_expression(object, bindings),
        HirAssignmentTarget::ComputedMember { object, property } => {
            collect_expression(object, bindings);
            collect_expression(property, bindings);
        }
        HirAssignmentTarget::PrivateMember { object, .. } => collect_expression(object, bindings),
    }
}

/// Walks every expression-owned child and records class bindings before their nested definitions.
fn collect_expression(expression: &HirExpression, bindings: &mut Vec<ClassEnvironment>) {
    match &expression.kind {
        HirExpressionKind::Class(class) => {
            if class.name_binding.is_some() || !class.private_names.is_empty() {
                let mut name_binding = class.name_binding.clone();
                if let Some(binding) = &mut name_binding {
                    binding.scope = class.scope;
                }
                bindings.push(ClassEnvironment {
                    scope: class.scope,
                    name_binding,
                    private_names: class.private_names.as_ref().into(),
                });
            }
            if let Some(super_class) = &class.super_class {
                collect_expression(super_class, bindings);
            }
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
                    collect_expression(key, bindings);
                }
            }
        }
        HirExpressionKind::Sequence(expressions) => {
            for expression in expressions.iter() {
                collect_expression(expression, bindings);
            }
        }
        HirExpressionKind::Object(properties) | HirExpressionKind::Array(properties) => {
            for property in properties.iter() {
                collect_property(&property.key, &property.value, bindings);
            }
        }
        HirExpressionKind::ObjectSpread(parts) => {
            for part in parts.iter() {
                match part {
                    HirObjectExpressionPart::Property(property) => {
                        collect_property(&property.key, &property.value, bindings);
                    }
                    HirObjectExpressionPart::Spread(expression) => {
                        collect_expression(expression, bindings);
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
            collect_pattern(target, bindings);
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
        | HirExpressionKind::SuperStaticMember(_) => {}
    }
}

fn collect_property(
    key: &HirObjectPropertyKey,
    value: &HirObjectPropertyValue,
    bindings: &mut Vec<ClassEnvironment>,
) {
    if let HirObjectPropertyKey::Computed(key) = key {
        collect_expression(key, bindings);
    }
    if let HirObjectPropertyValue::Data(value) = value {
        collect_expression(value, bindings);
    }
}
