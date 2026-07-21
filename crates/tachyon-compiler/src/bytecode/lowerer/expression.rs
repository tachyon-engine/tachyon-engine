use super::*;

#[derive(Clone, Copy)]
enum LoweredClassKey {
    Static(u32),
    Computed(RegisterId),
}

#[derive(Clone, Copy)]
struct PendingStaticField {
    key: LoweredClassKey,
    initializer: Option<RegisterId>,
    infer_name: bool,
    span: SourceSpan,
}

#[derive(Clone, Copy)]
struct PendingInstanceField {
    key: LoweredClassKey,
    initializer: Option<RegisterId>,
    infer_name: bool,
    span: SourceSpan,
}

impl Lowerer<'_> {
    /// Lowers expressions into registers while leaving unsupported reference semantics as explicit errors.
    pub(in crate::bytecode) fn expression(
        &mut self,
        expression: &HirExpression,
    ) -> Result<RegisterId, CompileError> {
        match &expression.kind {
            HirExpressionKind::Number(bits) => {
                let value = f64::from_bits(*bits);
                if value.is_finite()
                    && value.fract() == 0.0
                    && value >= i32::MIN as f64
                    && value <= i32::MAX as f64
                {
                    self.load_immediate(value as i32 as u32, expression.span)
                } else {
                    let register = self.register()?;
                    let constant = u32::try_from(self.constants.len())
                        .map_err(|_| CompileError::ConstantOverflow)?;
                    self.constants.push(BytecodeConstant::NumberBits(*bits));
                    self.emit(
                        Opcode::LoadConstant,
                        &[register.index(), constant],
                        expression.span,
                    )?;
                    Ok(register)
                }
            }
            HirExpressionKind::String(value) => {
                let code_unit_count = value.encode_utf16().count();
                let mut code_units = Vec::new();
                code_units
                    .try_reserve_exact(code_unit_count)
                    .map_err(|_| CompileError::ConstantAllocationFailed)?;
                code_units.extend(value.encode_utf16());
                let constant = u32::try_from(self.constants.len())
                    .map_err(|_| CompileError::ConstantOverflow)?;
                self.constants
                    .push(BytecodeConstant::string_from_utf16(code_units));
                let destination = self.register()?;
                self.emit(
                    Opcode::LoadConstant,
                    &[destination.index(), constant],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::RegExp { pattern, flags } => {
                let mut code_units = Vec::new();
                code_units
                    .try_reserve_exact(pattern.encode_utf16().count())
                    .map_err(|_| CompileError::ConstantAllocationFailed)?;
                code_units.extend(pattern.encode_utf16());
                let constant = u32::try_from(self.constants.len())
                    .map_err(|_| CompileError::ConstantOverflow)?;
                self.constants
                    .push(BytecodeConstant::regexp_from_utf16(code_units, *flags));
                let destination = self.register()?;
                self.emit(
                    Opcode::LoadConstant,
                    &[destination.index(), constant],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Boolean(value) => self.load_boolean(*value, expression.span),
            HirExpressionKind::Null => self.load_null(expression.span),
            HirExpressionKind::Unary {
                operator: HirUnaryOperator::Not,
                argument,
            } => {
                let argument = self.expression(argument)?;
                let destination = self.register()?;
                self.emit(
                    Opcode::Not,
                    &[destination.index(), argument.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Unary {
                operator: HirUnaryOperator::Negate,
                argument,
            } => {
                let argument = self.expression(argument)?;
                let destination = self.register()?;
                self.emit(
                    Opcode::Negate,
                    &[destination.index(), argument.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Unary {
                operator: HirUnaryOperator::Typeof,
                argument,
            } => {
                if let HirExpressionKind::Identifier(reference) = &argument.kind
                    && self.local_reference(reference).is_none()
                    && self.captured_reference(reference)?.is_none()
                {
                    self.require_global_reference(reference, expression.span)?;
                    let destination = self.register()?;
                    let scope_name = self.resolved_global_binding(reference, false)?;
                    self.emit(
                        Opcode::TypeofScope,
                        &[destination.index(), scope_name],
                        expression.span,
                    )?;
                    return Ok(destination);
                }
                let argument = self.expression(argument)?;
                let destination = self.register()?;
                self.emit(
                    Opcode::Typeof,
                    &[destination.index(), argument.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Unary {
                operator: HirUnaryOperator::Void,
                argument,
            } => {
                self.expression(argument)?;
                self.load_undefined(expression.span)
            }
            HirExpressionKind::Unary {
                operator: HirUnaryOperator::Delete,
                argument,
            } => match &argument.kind {
                HirExpressionKind::StaticMember { object, property } => {
                    let receiver = self.expression(object)?;
                    let destination = self.register()?;
                    let property = self.scope_name(property)?;
                    self.emit(
                        Opcode::DeleteById,
                        &[destination.index(), receiver.index(), property],
                        expression.span,
                    )?;
                    Ok(destination)
                }
                HirExpressionKind::ComputedMember { object, property } => {
                    let receiver = self.expression(object)?;
                    let property = self.expression(property)?;
                    self.prepare_property_key(property, receiver, false, expression.span)?;
                    let destination = self.register()?;
                    self.emit(
                        Opcode::DeleteByValue,
                        &[destination.index(), receiver.index(), property.index()],
                        expression.span,
                    )?;
                    Ok(destination)
                }
                HirExpressionKind::Identifier(_) => self.load_boolean(true, expression.span),
                _ => {
                    self.expression(argument)?;
                    self.load_boolean(true, expression.span)
                }
            },
            HirExpressionKind::Unary {
                operator: HirUnaryOperator::Plus,
                argument,
            } => {
                let argument = self.expression(argument)?;
                let destination = self.register()?;
                self.emit(
                    Opcode::ToNumber,
                    &[destination.index(), argument.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Unary {
                operator: HirUnaryOperator::BitwiseNot,
                argument,
            } => {
                let argument = self.expression(argument)?;
                let destination = self.register()?;
                self.emit(
                    Opcode::BitwiseNot,
                    &[destination.index(), argument.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Binary {
                operator,
                left,
                right,
            } => {
                let left = self.expression(left)?;
                let right = self.expression(right)?;
                self.emit_binary(*operator, left, right, expression.span)
            }
            HirExpressionKind::Logical {
                operator,
                left,
                right,
            } => self.logical(*operator, left, right, expression.span),
            HirExpressionKind::Identifier(reference) => {
                if reference.binding.is_none()
                    && reference.name.as_ref() == "arguments"
                    && self.function_scope.is_some()
                {
                    let destination = self.register()?;
                    self.emit(
                        Opcode::LoadArgumentsObject,
                        &[destination.index()],
                        expression.span,
                    )?;
                    return Ok(destination);
                }
                match self.local_reference(reference).cloned() {
                    Some(binding) => self.read_local(&binding, expression.span),
                    None => {
                        if let Some(binding) = self.captured_reference(reference)? {
                            return self.read_local(&binding, expression.span);
                        }
                        self.require_global_reference(reference, expression.span)?;
                        let destination = self.register()?;
                        let scope_name = self.resolved_global_binding(reference, true)?;
                        self.emit(
                            Opcode::LoadScope,
                            &[destination.index(), scope_name],
                            expression.span,
                        )?;
                        Ok(destination)
                    }
                }
            }
            HirExpressionKind::Function(function) => {
                let destination = self.register()?;
                let function = function
                    .index()
                    .checked_add(1)
                    .ok_or(CompileError::RegisterOverflow)?;
                self.emit(
                    Opcode::CreateClosure,
                    &[destination.index(), function],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Class(class) => self.class_expression(class, expression.span),
            HirExpressionKind::This => {
                let destination = self.register()?;
                self.emit(Opcode::LoadThis, &[destination.index()], expression.span)?;
                Ok(destination)
            }
            HirExpressionKind::NewTarget => {
                let destination = self.register()?;
                self.emit(
                    Opcode::LoadNewTarget,
                    &[destination.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Sequence(expressions) => {
                let mut result = None;
                for expression in expressions.iter() {
                    result = Some(self.expression(expression)?);
                }
                result.ok_or_else(|| CompileError::UnsupportedSyntax {
                    source_name: self.source_name.clone(),
                    span: expression.span,
                    syntax: "empty sequence expression",
                })
            }
            HirExpressionKind::Object(properties) | HirExpressionKind::Array(properties) => {
                let object = self.register()?;
                let create = if matches!(&expression.kind, HirExpressionKind::Array(_)) {
                    Opcode::CreateArray
                } else {
                    Opcode::CreateObject
                };
                self.emit(create, &[object.index()], expression.span)?;
                for property in properties.iter() {
                    let (opcode, key) = match &property.key {
                        HirObjectPropertyKey::Static(key) => {
                            (Opcode::SetById, self.scope_name(key)?)
                        }
                        HirObjectPropertyKey::Computed(key) => {
                            let key = self.expression(key)?;
                            self.prepare_property_key(key, object, false, property.span)?;
                            (Opcode::SetByValue, key.index())
                        }
                    };
                    match &property.value {
                        HirObjectPropertyValue::Data(value) => {
                            let value = self.expression(value)?;
                            self.emit(
                                opcode,
                                &[object.index(), value.index(), key],
                                property.span,
                            )?;
                        }
                        HirObjectPropertyValue::Getter(function)
                        | HirObjectPropertyValue::Setter(function) => {
                            let value = self.register()?;
                            let function = function
                                .index()
                                .checked_add(1)
                                .ok_or(CompileError::RegisterOverflow)?;
                            self.emit(
                                Opcode::CreateClosure,
                                &[value.index(), function],
                                property.span,
                            )?;
                            let (static_opcode, value_opcode, is_getter, prefix) =
                                match &property.value {
                                    HirObjectPropertyValue::Getter(_) => (
                                        Opcode::DefineGetterById,
                                        Opcode::DefineGetterByValue,
                                        true,
                                        "get ",
                                    ),
                                    HirObjectPropertyValue::Setter(_) => (
                                        Opcode::DefineSetterById,
                                        Opcode::DefineSetterByValue,
                                        false,
                                        "set ",
                                    ),
                                    HirObjectPropertyValue::Data(_) => {
                                        unreachable!("accessor arm selected")
                                    }
                                };
                            match &property.key {
                                HirObjectPropertyKey::Static(key_name) => {
                                    let function_name =
                                        std::sync::Arc::from(format!("{prefix}{key_name}"));
                                    let function_name = self.scope_name(&function_name)?;
                                    self.emit(
                                        Opcode::SetFunctionName,
                                        &[value.index(), function_name],
                                        property.span,
                                    )?;
                                    self.emit(
                                        static_opcode,
                                        &[object.index(), value.index(), key],
                                        property.span,
                                    )?;
                                }
                                HirObjectPropertyKey::Computed(_) => {
                                    self.emit(
                                        Opcode::SetAccessorFunctionName,
                                        &[value.index(), key, u32::from(is_getter)],
                                        property.span,
                                    )?;
                                    self.emit(
                                        value_opcode,
                                        &[object.index(), value.index(), key],
                                        property.span,
                                    )?;
                                }
                            }
                        }
                    }
                }
                Ok(object)
            }
            HirExpressionKind::ObjectSpread(parts) => {
                let object = self.register()?;
                self.emit(Opcode::CreateObject, &[object.index()], expression.span)?;
                let exclusions = self.create_exclusion_list(0, expression.span)?;
                for part in parts.iter() {
                    match part {
                        HirObjectExpressionPart::Spread(source) => {
                            let span = source.span;
                            let source = self.expression(source)?;
                            self.emit(
                                Opcode::CopyDataProperties,
                                &[object.index(), source.index(), exclusions.index()],
                                span,
                            )?;
                        }
                        HirObjectExpressionPart::Property(property) => {
                            let (opcode, key) = match &property.key {
                                HirObjectPropertyKey::Static(key) => {
                                    (Opcode::SetById, self.scope_name(key)?)
                                }
                                HirObjectPropertyKey::Computed(key) => {
                                    let key = self.expression(key)?;
                                    self.prepare_property_key(key, object, false, property.span)?;
                                    (Opcode::SetByValue, key.index())
                                }
                            };
                            match &property.value {
                                HirObjectPropertyValue::Data(value) => {
                                    let value = self.expression(value)?;
                                    self.emit(
                                        opcode,
                                        &[object.index(), value.index(), key],
                                        property.span,
                                    )?;
                                }
                                HirObjectPropertyValue::Getter(_)
                                | HirObjectPropertyValue::Setter(_) => {
                                    return Err(self.unsupported(
                                        property.span,
                                        "object spread with accessor property",
                                    ));
                                }
                            }
                        }
                    }
                }
                Ok(object)
            }
            HirExpressionKind::StaticMember { object, property } => {
                if self.function_scope.is_some()
                    && property.as_ref() == "length"
                    && matches!(
                        &object.kind,
                        HirExpressionKind::Identifier(reference)
                            if reference.binding.is_none() && reference.name.as_ref() == "arguments"
                    )
                {
                    let destination = self.register()?;
                    self.emit(
                        Opcode::LoadArgumentsLength,
                        &[destination.index()],
                        expression.span,
                    )?;
                    return Ok(destination);
                }
                let receiver = self.expression(object)?;
                let destination = self.register()?;
                let property = self.scope_name(property)?;
                self.emit(
                    Opcode::GetById,
                    &[destination.index(), receiver.index(), property],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::ComputedMember { object, property } => {
                let receiver = self.expression(object)?;
                let property = self.expression(property)?;
                self.prepare_property_key(property, receiver, false, expression.span)?;
                let destination = self.register()?;
                self.emit(
                    Opcode::GetByValue,
                    &[destination.index(), receiver.index(), property.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::SuperStaticMember(property) => {
                let destination = self.register()?;
                let property = self.scope_name(property)?;
                self.emit(
                    Opcode::GetSuperById,
                    &[destination.index(), property],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::SuperComputedMember(property) => {
                let base = self.register()?;
                self.emit(Opcode::LoadSuperBase, &[base.index()], expression.span)?;
                let property = self.expression(property)?;
                self.prepare_property_key(property, base, false, expression.span)?;
                let destination = self.register()?;
                self.emit(
                    Opcode::GetSuperByValue,
                    &[destination.index(), base.index(), property.index()],
                    expression.span,
                )?;
                Ok(destination)
            }
            HirExpressionKind::Assignment {
                operator,
                target,
                value,
            } => {
                if matches!(
                    target.kind,
                    HirPatternKind::Object { .. } | HirPatternKind::Array { .. }
                ) {
                    if *operator != HirAssignmentOperator::Assign {
                        return Err(
                            self.unsupported(target.span, "destructuring compound assignment")
                        );
                    }
                    let value = self.expression(value)?;
                    self.assign_pattern(target, value, expression.span)?;
                    Ok(value)
                } else {
                    let target = target.assignment_target().ok_or_else(|| {
                        self.unsupported(target.span, "destructuring pattern bytecode")
                    })?;
                    self.assignment_expression(*operator, target, value, expression.span)
                }
            }
            HirExpressionKind::Update {
                operator,
                prefix,
                target,
            } => self.update_expression(*operator, *prefix, target, expression.span),
            HirExpressionKind::Conditional {
                test,
                consequent,
                alternate,
            } => self.conditional(test, consequent, alternate, expression.span),
            HirExpressionKind::Call { callee, arguments } => {
                self.call_expression(callee, arguments, expression.span)
            }
            HirExpressionKind::SuperCall(arguments) => {
                self.super_call_expression(arguments, expression.span)
            }
            HirExpressionKind::New { callee, arguments } => {
                self.construct_expression(callee, arguments, expression.span)
            }
        }
    }

    /// Creates a base class directly or evaluates derived heritage once before publishing its pair.
    fn class_expression(
        &mut self,
        class: &crate::HirClass,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let previous_scope = self.active_scope;
        if class.name_binding.is_some() {
            self.emit(Opcode::EnterClassEnvironment, &[], span)?;
            self.environment_depth = self
                .environment_depth
                .checked_add(1)
                .ok_or(CompileError::BindingOverflow)?;
            self.active_scope = class.scope;
        }
        let prototype_name = self.scope_name(&std::sync::Arc::from("prototype"))?;
        let destination = self.register()?;
        let function = class
            .constructor
            .index()
            .checked_add(1)
            .ok_or(CompileError::RegisterOverflow)?;
        if let Some(super_class) = &class.super_class {
            let superclass = self.expression(super_class)?;
            let heritage_base = self.register()?;
            self.emit(
                Opcode::Move,
                &[heritage_base.index(), superclass.index()],
                super_class.span,
            )?;
            let superclass_prototype = self.register()?;
            self.emit(
                Opcode::CheckConstructor,
                &[heritage_base.index()],
                super_class.span,
            )?;
            self.emit(
                Opcode::GetById,
                &[
                    superclass_prototype.index(),
                    heritage_base.index(),
                    prototype_name,
                ],
                super_class.span,
            )?;
            self.emit(
                Opcode::CreateClass,
                &[destination.index(), function, heritage_base.index()],
                span,
            )?;
        } else {
            self.emit(
                Opcode::CreateBaseClass,
                &[destination.index(), function],
                span,
            )?;
        }
        if let Some(name) = &class.name {
            let name = self.scope_name(name)?;
            self.emit(Opcode::SetFunctionName, &[destination.index(), name], span)?;
        }
        let instance_target = if class.elements.iter().any(|element| match element {
            crate::HirClassElement::Method(method) => !method.is_static,
            crate::HirClassElement::PublicField(field) => !field.is_static,
        }) {
            let target = self.register()?;
            self.emit(
                Opcode::GetById,
                &[target.index(), destination.index(), prototype_name],
                span,
            )?;
            Some(target)
        } else {
            None
        };
        let static_field_count = class
            .elements
            .iter()
            .filter(|element| {
                matches!(element, crate::HirClassElement::PublicField(field) if field.is_static)
            })
            .count();
        let mut static_fields = Vec::with_capacity(static_field_count);
        let instance_field_count = class
            .elements
            .iter()
            .filter(|element| {
                matches!(element, crate::HirClassElement::PublicField(field) if !field.is_static)
            })
            .count();
        let mut instance_fields = Vec::with_capacity(instance_field_count);
        for element in class.elements.iter() {
            match element {
                crate::HirClassElement::Method(method) => {
                    let target = if method.is_static {
                        destination
                    } else {
                        instance_target
                            .expect("instance target exists when an instance method was counted")
                    };
                    self.define_class_method(method, target)?;
                }
                crate::HirClassElement::PublicField(field) => {
                    let key = self.lower_class_key(&field.key, destination)?;
                    let initializer = field
                        .initializer
                        .map(|initializer| {
                            let closure = self.register()?;
                            let function = initializer
                                .index()
                                .checked_add(1)
                                .ok_or(CompileError::RegisterOverflow)?;
                            self.emit(
                                Opcode::CreateClosure,
                                &[closure.index(), function],
                                field.span,
                            )?;
                            self.emit(
                                Opcode::SetFunctionHomeObject,
                                &[
                                    closure.index(),
                                    if field.is_static {
                                        destination.index()
                                    } else {
                                        instance_target
                                            .expect("instance field requires class prototype")
                                            .index()
                                    },
                                ],
                                field.span,
                            )?;
                            Ok::<RegisterId, CompileError>(closure)
                        })
                        .transpose()?;
                    if field.is_static {
                        static_fields.push(PendingStaticField {
                            key,
                            initializer,
                            infer_name: field.infer_name,
                            span: field.span,
                        });
                    } else {
                        instance_fields.push(PendingInstanceField {
                            key,
                            initializer,
                            infer_name: field.infer_name,
                            span: field.span,
                        });
                    }
                }
            }
        }
        if !instance_fields.is_empty() {
            self.attach_instance_fields(destination, &instance_fields, span)?;
        }
        if class.name_binding.is_some() {
            self.emit(
                Opcode::InitializeClassEnvironment,
                &[destination.index()],
                span,
            )?;
        }
        for field in static_fields {
            self.initialize_static_field(destination, field)?;
        }
        if class.name_binding.is_some() {
            self.emit(Opcode::LeaveClassEnvironment, &[], span)?;
            self.environment_depth = self
                .environment_depth
                .checked_sub(1)
                .ok_or(CompileError::BindingOverflow)?;
            self.active_scope = previous_scope;
        }
        Ok(destination)
    }

    /// Evaluates one class key in source order, names its closure, and installs its descriptor.
    fn define_class_method(
        &mut self,
        method: &crate::HirClassMethod,
        target: RegisterId,
    ) -> Result<(), CompileError> {
        let key = self.lower_class_key(&method.key, target)?;
        let closure = self.register()?;
        let function = method
            .function
            .index()
            .checked_add(1)
            .ok_or(CompileError::RegisterOverflow)?;
        self.emit(
            Opcode::CreateClosure,
            &[closure.index(), function],
            method.span,
        )?;
        if let LoweredClassKey::Computed(key) = key {
            match method.kind {
                crate::HirClassMethodKind::Method => self.emit(
                    Opcode::SetFunctionNameByValue,
                    &[closure.index(), key.index()],
                    method.span,
                )?,
                crate::HirClassMethodKind::Getter | crate::HirClassMethodKind::Setter => self
                    .emit(
                        Opcode::SetAccessorFunctionName,
                        &[
                            closure.index(),
                            key.index(),
                            u32::from(method.kind == crate::HirClassMethodKind::Getter),
                        ],
                        method.span,
                    )?,
            }
        }
        let opcode = match (method.kind, key) {
            (crate::HirClassMethodKind::Method, LoweredClassKey::Static(_)) => {
                Opcode::DefineClassMethodById
            }
            (crate::HirClassMethodKind::Getter, LoweredClassKey::Static(_)) => {
                Opcode::DefineClassGetterById
            }
            (crate::HirClassMethodKind::Setter, LoweredClassKey::Static(_)) => {
                Opcode::DefineClassSetterById
            }
            (crate::HirClassMethodKind::Method, LoweredClassKey::Computed(_)) => {
                Opcode::DefineClassMethodByValue
            }
            (crate::HirClassMethodKind::Getter, LoweredClassKey::Computed(_)) => {
                Opcode::DefineClassGetterByValue
            }
            (crate::HirClassMethodKind::Setter, LoweredClassKey::Computed(_)) => {
                Opcode::DefineClassSetterByValue
            }
        };
        let key = match key {
            LoweredClassKey::Static(name) => name,
            LoweredClassKey::Computed(key) => key.index(),
        };
        self.emit(opcode, &[target.index(), closure.index(), key], method.span)
    }

    /// Evaluates a computed class key exactly once while its class-definition environment is active.
    fn lower_class_key(
        &mut self,
        key: &HirObjectPropertyKey,
        target: RegisterId,
    ) -> Result<LoweredClassKey, CompileError> {
        match key {
            HirObjectPropertyKey::Static(name) => {
                Ok(LoweredClassKey::Static(self.scope_name(name)?))
            }
            HirObjectPropertyKey::Computed(expression) => {
                let key = self.expression(expression)?;
                self.prepare_property_key(key, target, false, expression.span)?;
                Ok(LoweredClassKey::Computed(key))
            }
        }
    }

    /// Calls one hidden static-field initializer only after every class key has been evaluated.
    fn initialize_static_field(
        &mut self,
        target: RegisterId,
        field: PendingStaticField,
    ) -> Result<(), CompileError> {
        let value = if let Some(initializer) = field.initializer {
            let receiver = self.register()?;
            let callee = self.register()?;
            debug_assert_eq!(callee.index(), receiver.index() + 1);
            self.emit(
                Opcode::Move,
                &[receiver.index(), target.index()],
                field.span,
            )?;
            self.emit(
                Opcode::Move,
                &[callee.index(), initializer.index()],
                field.span,
            )?;
            let value = self.register()?;
            self.emit(
                Opcode::CallWithReceiver,
                &[value.index(), receiver.index(), 0],
                field.span,
            )?;
            value
        } else {
            self.load_undefined(field.span)?
        };
        if field.infer_name {
            match field.key {
                LoweredClassKey::Static(name) => {
                    self.emit(Opcode::SetFunctionName, &[value.index(), name], field.span)?;
                }
                LoweredClassKey::Computed(key) => {
                    self.emit(
                        Opcode::SetFunctionNameByValue,
                        &[value.index(), key.index()],
                        field.span,
                    )?;
                }
            }
        }
        match field.key {
            LoweredClassKey::Static(name) => self.emit(
                Opcode::DefineFieldById,
                &[target.index(), value.index(), name],
                field.span,
            ),
            LoweredClassKey::Computed(key) => self.emit(
                Opcode::DefineFieldByValue,
                &[target.index(), value.index(), key.index()],
                field.span,
            ),
        }
    }

    /// Freezes key/initializer/name triples into one verified contiguous constructor record window.
    fn attach_instance_fields(
        &mut self,
        constructor: RegisterId,
        fields: &[PendingInstanceField],
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let count = u32::try_from(fields.len()).map_err(|_| CompileError::RegisterOverflow)?;
        let register_count = count.checked_mul(3).ok_or(CompileError::RegisterOverflow)?;
        let record_base = self.register()?;
        for _ in 1..register_count {
            self.register()?;
        }
        for (index, field) in fields.iter().enumerate() {
            let offset = u32::try_from(index)
                .map_err(|_| CompileError::RegisterOverflow)?
                .checked_mul(3)
                .ok_or(CompileError::RegisterOverflow)?;
            let key_slot = record_base
                .index()
                .checked_add(offset)
                .ok_or(CompileError::RegisterOverflow)?;
            let initializer_slot = key_slot
                .checked_add(1)
                .ok_or(CompileError::RegisterOverflow)?;
            let infer_name_slot = key_slot
                .checked_add(2)
                .ok_or(CompileError::RegisterOverflow)?;
            let key = self.class_key_value(field.key, field.span)?;
            self.emit(Opcode::Move, &[key_slot, key.index()], field.span)?;
            let initializer = match field.initializer {
                Some(initializer) => initializer,
                None => self.load_undefined(field.span)?,
            };
            self.emit(
                Opcode::Move,
                &[initializer_slot, initializer.index()],
                field.span,
            )?;
            self.emit(
                if field.infer_name {
                    Opcode::LoadTrue
                } else {
                    Opcode::LoadFalse
                },
                &[infer_name_slot],
                field.span,
            )?;
        }
        self.emit(
            Opcode::AttachInstanceFields,
            &[constructor.index(), record_base.index(), count],
            span,
        )
    }

    /// Materializes a static class key as the same string Value shape produced by ToPropertyKey.
    fn class_key_value(
        &mut self,
        key: LoweredClassKey,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let LoweredClassKey::Static(name) = key else {
            let LoweredClassKey::Computed(key) = key else {
                unreachable!("class key variants are exhaustive")
            };
            return Ok(key);
        };
        let name = self
            .scope_names
            .get(name as usize)
            .expect("lowered scope-name index remains published")
            .clone();
        let mut code_units = Vec::new();
        code_units
            .try_reserve_exact(name.encode_utf16().count())
            .map_err(|_| CompileError::ConstantAllocationFailed)?;
        code_units.extend(name.encode_utf16());
        let constant =
            u32::try_from(self.constants.len()).map_err(|_| CompileError::ConstantOverflow)?;
        self.constants
            .push(BytecodeConstant::string_from_utf16(code_units));
        let destination = self.register()?;
        self.emit(Opcode::LoadConstant, &[destination.index(), constant], span)?;
        Ok(destination)
    }

    /// Evaluates super arguments into one verified contiguous window before construction.
    fn super_call_expression(
        &mut self,
        arguments: &[HirExpression],
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let argument_base = self.register()?;
        let mut slots = Vec::with_capacity(arguments.len());
        for _ in arguments {
            slots.push(self.register()?);
        }
        for (argument, slot) in arguments.iter().zip(slots) {
            let value = self.expression(argument)?;
            self.emit(Opcode::Move, &[slot.index(), value.index()], argument.span)?;
        }
        let destination = self.register()?;
        let count = u32::try_from(arguments.len()).map_err(|_| CompileError::RegisterOverflow)?;
        self.emit(
            Opcode::SuperConstruct,
            &[destination.index(), argument_base.index(), count],
            span,
        )?;
        self.emit(Opcode::InitializeThis, &[destination.index()], span)?;
        if self.initialize_instance_elements {
            self.emit(
                Opcode::InitializeInstanceElements,
                &[destination.index()],
                span,
            )?;
        }
        Ok(destination)
    }

    /// Reads a compound target before its RHS, computes once, and publishes the resulting value.
    pub(in crate::bytecode) fn assignment_expression(
        &mut self,
        operator: HirAssignmentOperator,
        target: &HirAssignmentTarget,
        value: &HirExpression,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        match target {
            HirAssignmentTarget::Identifier(target) => {
                if let Some(binding) = self.local_reference(target).cloned() {
                    if !binding.mutable && matches!(binding.storage, LocalStorage::Register(_)) {
                        return Err(self.unsupported(span, "assignment to immutable local"));
                    }
                    let result = match operator {
                        HirAssignmentOperator::Assign => self.expression(value)?,
                        HirAssignmentOperator::Binary(operator) => {
                            let old_value = self.snapshot_local(&binding, span)?;
                            let right = self.expression(value)?;
                            self.emit_binary(operator, old_value, right, span)?
                        }
                        HirAssignmentOperator::Logical(operator) => {
                            let old_value = self.snapshot_local(&binding, span)?;
                            self.logical_assignment(operator, old_value, value, span, None)?
                        }
                    };
                    self.write_local(&binding, result, span)?;
                    return Ok(result);
                }
                if let Some(binding) = self.captured_reference(target)? {
                    if !binding.mutable && matches!(binding.storage, LocalStorage::Register(_)) {
                        return Err(self.unsupported(span, "assignment to immutable capture"));
                    }
                    let result = match operator {
                        HirAssignmentOperator::Assign => self.expression(value)?,
                        HirAssignmentOperator::Binary(operator) => {
                            let old_value = self.snapshot_local(&binding, span)?;
                            let right = self.expression(value)?;
                            self.emit_binary(operator, old_value, right, span)?
                        }
                        HirAssignmentOperator::Logical(operator) => {
                            let old_value = self.snapshot_local(&binding, span)?;
                            self.logical_assignment(operator, old_value, value, span, None)?
                        }
                    };
                    self.write_local(&binding, result, span)?;
                    return Ok(result);
                }
                self.scope_assignment(operator, target, value, span)
            }
            HirAssignmentTarget::StaticMember { object, property } => {
                let receiver = self.expression(object)?;
                let property = self.scope_name(property)?;
                let result = match operator {
                    HirAssignmentOperator::Assign => self.expression(value)?,
                    HirAssignmentOperator::Binary(operator) => {
                        let old_value = self.register()?;
                        self.emit(
                            Opcode::GetById,
                            &[old_value.index(), receiver.index(), property],
                            span,
                        )?;
                        let right = self.expression(value)?;
                        self.emit_binary(operator, old_value, right, span)?
                    }
                    HirAssignmentOperator::Logical(operator) => {
                        let old_value = self.register()?;
                        self.emit(
                            Opcode::GetById,
                            &[old_value.index(), receiver.index(), property],
                            span,
                        )?;
                        self.logical_assignment(
                            operator,
                            old_value,
                            value,
                            span,
                            Some((Opcode::SetById, receiver.index(), property)),
                        )?
                    }
                };
                if !matches!(operator, HirAssignmentOperator::Logical(_)) {
                    self.emit(
                        Opcode::SetById,
                        &[receiver.index(), result.index(), property],
                        span,
                    )?;
                }
                Ok(result)
            }
            HirAssignmentTarget::ComputedMember { object, property } => {
                let receiver = self.expression(object)?;
                let property = self.expression(property)?;
                let result = match operator {
                    HirAssignmentOperator::Assign => {
                        let value = self.expression(value)?;
                        self.prepare_property_key(property, receiver, false, span)?;
                        value
                    }
                    HirAssignmentOperator::Binary(operator) => {
                        self.prepare_property_key(property, receiver, false, span)?;
                        let old_value = self.register()?;
                        self.emit(
                            Opcode::GetByValue,
                            &[old_value.index(), receiver.index(), property.index()],
                            span,
                        )?;
                        let right = self.expression(value)?;
                        self.emit_binary(operator, old_value, right, span)?
                    }
                    HirAssignmentOperator::Logical(operator) => {
                        self.prepare_property_key(property, receiver, false, span)?;
                        let old_value = self.register()?;
                        self.emit(
                            Opcode::GetByValue,
                            &[old_value.index(), receiver.index(), property.index()],
                            span,
                        )?;
                        self.logical_assignment(
                            operator,
                            old_value,
                            value,
                            span,
                            Some((Opcode::SetByValue, receiver.index(), property.index())),
                        )?
                    }
                };
                if !matches!(operator, HirAssignmentOperator::Logical(_)) {
                    self.emit(
                        Opcode::SetByValue,
                        &[receiver.index(), result.index(), property.index()],
                        span,
                    )?;
                }
                Ok(result)
            }
        }
    }

    /// Preserves identifier-reference order while updating only an already resolved scope binding.
    pub(in crate::bytecode) fn scope_assignment(
        &mut self,
        operator: HirAssignmentOperator,
        target: &HirIdentifierReference,
        value: &HirExpression,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        self.require_global_reference(target, span)?;
        let scope_name = self.resolved_global_binding(target, true)?;
        let result = match operator {
            HirAssignmentOperator::Assign => self.expression(value)?,
            HirAssignmentOperator::Binary(operator) => {
                let old_value = self.register()?;
                self.emit(Opcode::LoadScope, &[old_value.index(), scope_name], span)?;
                let right = self.expression(value)?;
                self.emit_binary(operator, old_value, right, span)?
            }
            HirAssignmentOperator::Logical(operator) => {
                let old_value = self.register()?;
                self.emit(Opcode::LoadScope, &[old_value.index(), scope_name], span)?;
                self.logical_assignment(operator, old_value, value, span, None)?
            }
        };
        self.emit(
            Opcode::StoreResolvedScope,
            &[result.index(), scope_name],
            span,
        )?;
        Ok(result)
    }

    /// Emits one supported binary operation over already evaluated values.
    pub(in crate::bytecode) fn emit_binary(
        &mut self,
        operator: HirBinaryOperator,
        left: RegisterId,
        right: RegisterId,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        if matches!(
            operator,
            HirBinaryOperator::Equal
                | HirBinaryOperator::NotEqual
                | HirBinaryOperator::StrictNotEqual
        ) {
            let equal = self.register()?;
            let equality = if operator == HirBinaryOperator::StrictNotEqual {
                Opcode::StrictEqual
            } else {
                Opcode::LooseEqual
            };
            self.emit(
                equality,
                &[equal.index(), left.index(), right.index()],
                span,
            )?;
            if operator != HirBinaryOperator::Equal {
                let destination = self.register()?;
                self.emit(Opcode::Not, &[destination.index(), equal.index()], span)?;
                return Ok(destination);
            }
            return Ok(equal);
        }
        let opcode = match operator {
            HirBinaryOperator::Add => Opcode::Add,
            HirBinaryOperator::Subtract => Opcode::Sub,
            HirBinaryOperator::Multiply => Opcode::Mul,
            HirBinaryOperator::Divide => Opcode::Div,
            HirBinaryOperator::Remainder => Opcode::Remainder,
            HirBinaryOperator::Exponentiate => Opcode::Exponentiate,
            HirBinaryOperator::BitwiseAnd => Opcode::BitwiseAnd,
            HirBinaryOperator::BitwiseOr => Opcode::BitwiseOr,
            HirBinaryOperator::BitwiseXor => Opcode::BitwiseXor,
            HirBinaryOperator::ShiftLeft => Opcode::ShiftLeft,
            HirBinaryOperator::ShiftRight => Opcode::ShiftRight,
            HirBinaryOperator::ShiftRightUnsigned => Opcode::ShiftRightUnsigned,
            HirBinaryOperator::StrictEqual => Opcode::StrictEqual,
            HirBinaryOperator::LessThan => Opcode::LessThan,
            HirBinaryOperator::GreaterThan => Opcode::GreaterThan,
            HirBinaryOperator::LessEqual => Opcode::LessEqual,
            HirBinaryOperator::GreaterEqual => Opcode::GreaterEqual,
            HirBinaryOperator::InstanceOf => Opcode::InstanceOf,
            HirBinaryOperator::In => Opcode::HasProperty,
            _ => {
                return Err(CompileError::UnsupportedSyntax {
                    source_name: self.source_name.clone(),
                    span,
                    syntax: "binary operator",
                });
            }
        };
        if operator == HirBinaryOperator::In {
            self.prepare_property_key(left, right, true, span)?;
        }
        let destination = self.register()?;
        self.emit(
            opcode,
            &[destination.index(), left.index(), right.index()],
            span,
        )?;
        Ok(destination)
    }

    /// Converts one computed key in place after applying the operation's required base guard.
    pub(in crate::bytecode) fn prepare_property_key(
        &mut self,
        key: RegisterId,
        base: RegisterId,
        for_in_operator: bool,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        self.emit(
            if for_in_operator {
                Opcode::ToPropertyKeyForIn
            } else {
                Opcode::ToPropertyKey
            },
            &[key.index(), key.index(), base.index()],
            span,
        )
    }

    /// Reads one update reference once and preserves the prefix/postfix result distinction.
    pub(in crate::bytecode) fn update_expression(
        &mut self,
        operator: HirUpdateOperator,
        prefix: bool,
        target: &HirAssignmentTarget,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let opcode = match operator {
            HirUpdateOperator::Increment => HirBinaryOperator::Add,
            HirUpdateOperator::Decrement => HirBinaryOperator::Subtract,
        };
        match target {
            HirAssignmentTarget::Identifier(target) => {
                if let Some(binding) = self.local_reference(target).cloned() {
                    if !binding.mutable && matches!(binding.storage, LocalStorage::Register(_)) {
                        return Err(self.unsupported(span, "update of immutable local"));
                    }
                    let old = self.snapshot_local(&binding, span)?;
                    let one = self.load_immediate(1, span)?;
                    let updated = self.emit_binary(opcode, old, one, span)?;
                    self.write_local(&binding, updated, span)?;
                    return Ok(if prefix { updated } else { old });
                }
                if let Some(binding) = self.captured_reference(target)? {
                    if !binding.mutable && matches!(binding.storage, LocalStorage::Register(_)) {
                        return Err(self.unsupported(span, "update of immutable capture"));
                    }
                    let old = self.snapshot_local(&binding, span)?;
                    let one = self.load_immediate(1, span)?;
                    let updated = self.emit_binary(opcode, old, one, span)?;
                    self.write_local(&binding, updated, span)?;
                    return Ok(if prefix { updated } else { old });
                }
                self.scope_update(opcode, prefix, target, span)
            }
            HirAssignmentTarget::StaticMember { object, property } => {
                let receiver = self.expression(object)?;
                let property = self.scope_name(property)?;
                let old = self.register()?;
                self.emit(
                    Opcode::GetById,
                    &[old.index(), receiver.index(), property],
                    span,
                )?;
                let result = if prefix {
                    None
                } else {
                    let snapshot = self.register()?;
                    self.emit(Opcode::Move, &[snapshot.index(), old.index()], span)?;
                    Some(snapshot)
                };
                let one = self.load_immediate(1, span)?;
                let updated = self.emit_binary(opcode, old, one, span)?;
                self.emit(
                    Opcode::SetById,
                    &[receiver.index(), updated.index(), property],
                    span,
                )?;
                Ok(result.unwrap_or(updated))
            }
            HirAssignmentTarget::ComputedMember { object, property } => {
                let receiver = self.expression(object)?;
                let property = self.expression(property)?;
                self.prepare_property_key(property, receiver, false, span)?;
                let old = self.register()?;
                self.emit(
                    Opcode::GetByValue,
                    &[old.index(), receiver.index(), property.index()],
                    span,
                )?;
                let result = if prefix {
                    None
                } else {
                    let snapshot = self.register()?;
                    self.emit(Opcode::Move, &[snapshot.index(), old.index()], span)?;
                    Some(snapshot)
                };
                let one = self.load_immediate(1, span)?;
                let updated = self.emit_binary(opcode, old, one, span)?;
                self.emit(
                    Opcode::SetByValue,
                    &[receiver.index(), updated.index(), property.index()],
                    span,
                )?;
                Ok(result.unwrap_or(updated))
            }
        }
    }

    /// Loads, snapshots, updates, and stores one dynamically resolved identifier exactly once.
    pub(in crate::bytecode) fn scope_update(
        &mut self,
        opcode: HirBinaryOperator,
        prefix: bool,
        target: &HirIdentifierReference,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        self.require_global_reference(target, span)?;
        let scope_name = self.resolved_global_binding(target, true)?;
        let old = self.register()?;
        self.emit(Opcode::LoadScope, &[old.index(), scope_name], span)?;
        let result = if prefix {
            None
        } else {
            let snapshot = self.register()?;
            self.emit(Opcode::Move, &[snapshot.index(), old.index()], span)?;
            Some(snapshot)
        };
        let one = self.load_immediate(1, span)?;
        let updated = self.emit_binary(opcode, old, one, span)?;
        self.emit(
            Opcode::StoreResolvedScope,
            &[updated.index(), scope_name],
            span,
        )?;
        Ok(result.unwrap_or(updated))
    }

    /// Preserves the left operand value and evaluates the right operand only when required.
    pub(in crate::bytecode) fn logical(
        &mut self,
        operator: HirLogicalOperator,
        left: &HirExpression,
        right: &HirExpression,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let left = self.expression(left)?;
        let destination = self.register()?;
        self.emit(Opcode::Move, &[destination.index(), left.index()], span)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        let source_span = BytecodeSourceSpan {
            start: span.start,
            end: span.end,
        };
        match operator {
            HirLogicalOperator::And => self.builder.emit_jump_if_false(left, end, source_span),
            HirLogicalOperator::Or => self.builder.emit_jump_if_true(left, end, source_span),
            HirLogicalOperator::Coalesce => {
                self.builder
                    .emit_jump_if_not_nullish(left, end, source_span)
            }
        }
        .map_err(CompileError::Builder)?;
        let right = self.expression(right)?;
        self.emit(Opcode::Move, &[destination.index(), right.index()], span)?;
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        Ok(destination)
    }

    /// Preserves an assignment target's old value and evaluates RHS only when its logical test fails.
    pub(in crate::bytecode) fn logical_assignment(
        &mut self,
        operator: HirLogicalOperator,
        old: RegisterId,
        value: &HirExpression,
        span: SourceSpan,
        store: Option<(Opcode, u32, u32)>,
    ) -> Result<RegisterId, CompileError> {
        let destination = self.register()?;
        self.emit(Opcode::Move, &[destination.index(), old.index()], span)?;
        let end = self.builder.new_label().map_err(CompileError::Builder)?;
        let source_span = BytecodeSourceSpan {
            start: span.start,
            end: span.end,
        };
        match operator {
            HirLogicalOperator::And => self.builder.emit_jump_if_false(old, end, source_span),
            HirLogicalOperator::Or => self.builder.emit_jump_if_true(old, end, source_span),
            HirLogicalOperator::Coalesce => {
                self.builder.emit_jump_if_not_nullish(old, end, source_span)
            }
        }
        .map_err(CompileError::Builder)?;
        let right = self.expression(value)?;
        self.emit(Opcode::Move, &[destination.index(), right.index()], span)?;
        if let Some((opcode, receiver, property)) = store {
            self.emit(opcode, &[receiver, destination.index(), property], span)?;
        }
        self.builder
            .bind_label(end)
            .map_err(CompileError::Builder)?;
        Ok(destination)
    }

    /// Evaluates callee/arguments in source order and copies them into the verified contiguous call window.
    pub(in crate::bytecode) fn call_expression(
        &mut self,
        callee: &HirExpression,
        arguments: &[HirExpression],
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        if let HirExpressionKind::StaticMember { object, property } = &callee.kind {
            return self.method_call_expression(object, property, arguments, span);
        }
        if let HirExpressionKind::ComputedMember { object, property } = &callee.kind {
            return self.computed_method_call_expression(object, property, arguments, span);
        }
        if let HirExpressionKind::SuperStaticMember(property) = &callee.kind {
            return self.super_method_call_expression(Some(property), None, arguments, span);
        }
        if let HirExpressionKind::SuperComputedMember(property) = &callee.kind {
            return self.super_method_call_expression(None, Some(property), arguments, span);
        }
        let callee_value = self.expression(callee)?;
        if arguments.is_empty() {
            let destination = self.register()?;
            self.emit(
                Opcode::Call,
                &[destination.index(), callee_value.index(), 0],
                span,
            )?;
            return Ok(destination);
        }
        let call_base = self.register()?;
        self.emit(
            Opcode::Move,
            &[call_base.index(), callee_value.index()],
            span,
        )?;
        let mut argument_slots = Vec::with_capacity(arguments.len());
        for _ in arguments {
            argument_slots.push(self.register()?);
        }
        for (argument, slot) in arguments.iter().zip(argument_slots) {
            let value = self.expression(argument)?;
            self.emit(Opcode::Move, &[slot.index(), value.index()], argument.span)?;
        }
        let destination = self.register()?;
        let argument_count =
            u32::try_from(arguments.len()).map_err(|_| CompileError::RegisterOverflow)?;
        self.emit(
            Opcode::Call,
            &[destination.index(), call_base.index(), argument_count],
            span,
        )?;
        Ok(destination)
    }

    /// Loads a super method with the active `this` as receiver and preserves argument order.
    fn super_method_call_expression(
        &mut self,
        static_property: Option<&std::sync::Arc<str>>,
        computed_property: Option<&HirExpression>,
        arguments: &[HirExpression],
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let receiver_value = self.register()?;
        self.emit(Opcode::LoadThis, &[receiver_value.index()], span)?;
        let computed = if let Some(property) = computed_property {
            let base = self.register()?;
            self.emit(Opcode::LoadSuperBase, &[base.index()], span)?;
            let property = self.expression(property)?;
            self.prepare_property_key(property, base, false, span)?;
            Some((base, property))
        } else {
            None
        };
        let receiver = self.register()?;
        self.emit(
            Opcode::Move,
            &[receiver.index(), receiver_value.index()],
            span,
        )?;
        let callee = self.register()?;
        if let Some(property) = static_property {
            let property = self.scope_name(property)?;
            self.emit(Opcode::GetSuperById, &[callee.index(), property], span)?;
        } else {
            let (base, property) = computed.expect("computed super call retains its lookup state");
            self.emit(
                Opcode::GetSuperByValue,
                &[callee.index(), base.index(), property.index()],
                span,
            )?;
        }
        let mut argument_slots = Vec::with_capacity(arguments.len());
        for _ in arguments {
            argument_slots.push(self.register()?);
        }
        for (argument, slot) in arguments.iter().zip(argument_slots) {
            let value = self.expression(argument)?;
            self.emit(Opcode::Move, &[slot.index(), value.index()], argument.span)?;
        }
        let destination = self.register()?;
        let argument_count =
            u32::try_from(arguments.len()).map_err(|_| CompileError::RegisterOverflow)?;
        self.emit(
            Opcode::CallWithReceiver,
            &[destination.index(), receiver.index(), argument_count],
            span,
        )?;
        Ok(destination)
    }

    /// Uses an in-place local update when the surrounding statement discards its result.
    pub(in crate::bytecode) fn discarded_expression(
        &mut self,
        expression: &HirExpression,
    ) -> Result<(), CompileError> {
        if let HirExpressionKind::Update {
            operator,
            target: HirAssignmentTarget::Identifier(target),
            ..
        } = &expression.kind
            && let Some(binding) = self.local_reference(target).cloned()
            && let LocalStorage::Register(register) = binding.storage
        {
            if !binding.mutable {
                return Err(self.unsupported(expression.span, "update of immutable local"));
            }
            let one = self.load_immediate(1, expression.span)?;
            let opcode = match operator {
                HirUpdateOperator::Increment => Opcode::Add,
                HirUpdateOperator::Decrement => Opcode::Sub,
            };
            self.emit(
                opcode,
                &[register.index(), register.index(), one.index()],
                expression.span,
            )?;
            return Ok(());
        }
        self.expression(expression).map(|_| ())
    }

    /// Evaluates constructor and arguments once before emitting one verified construct window.
    pub(in crate::bytecode) fn construct_expression(
        &mut self,
        callee: &HirExpression,
        arguments: &[HirExpression],
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let callee_value = self.expression(callee)?;
        let call_base = self.register()?;
        self.emit(
            Opcode::Move,
            &[call_base.index(), callee_value.index()],
            span,
        )?;
        let mut argument_slots = Vec::with_capacity(arguments.len());
        for _ in arguments {
            argument_slots.push(self.register()?);
        }
        for (argument, slot) in arguments.iter().zip(argument_slots) {
            let value = self.expression(argument)?;
            self.emit(Opcode::Move, &[slot.index(), value.index()], argument.span)?;
        }
        let destination = self.register()?;
        let argument_count =
            u32::try_from(arguments.len()).map_err(|_| CompileError::RegisterOverflow)?;
        self.emit(
            Opcode::Construct,
            &[destination.index(), call_base.index(), argument_count],
            span,
        )?;
        Ok(destination)
    }

    /// Materializes receiver/callee/arguments once in one verified contiguous method-call window.
    pub(in crate::bytecode) fn method_call_expression(
        &mut self,
        object: &HirExpression,
        property: &std::sync::Arc<str>,
        arguments: &[HirExpression],
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let receiver_value = self.expression(object)?;
        let call_base = self.register()?;
        self.emit(
            Opcode::Move,
            &[call_base.index(), receiver_value.index()],
            span,
        )?;
        let callee_slot = self.register()?;
        let property = self.scope_name(property)?;
        self.emit(
            Opcode::GetById,
            &[callee_slot.index(), call_base.index(), property],
            span,
        )?;
        let mut argument_slots = Vec::with_capacity(arguments.len());
        for _ in arguments {
            argument_slots.push(self.register()?);
        }
        for (argument, slot) in arguments.iter().zip(argument_slots) {
            let value = self.expression(argument)?;
            self.emit(Opcode::Move, &[slot.index(), value.index()], argument.span)?;
        }
        let destination = self.register()?;
        let argument_count =
            u32::try_from(arguments.len()).map_err(|_| CompileError::RegisterOverflow)?;
        self.emit(
            Opcode::CallWithReceiver,
            &[destination.index(), call_base.index(), argument_count],
            span,
        )?;
        Ok(destination)
    }

    /// Prepares one computed key while retaining its receiver in the contiguous method-call window.
    pub(in crate::bytecode) fn computed_method_call_expression(
        &mut self,
        object: &HirExpression,
        property: &HirExpression,
        arguments: &[HirExpression],
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let receiver = self.expression(object)?;
        let property = self.expression(property)?;
        self.prepare_property_key(property, receiver, false, span)?;
        let call_base = self.register()?;
        self.emit(Opcode::Move, &[call_base.index(), receiver.index()], span)?;
        let callee = self.register()?;
        self.emit(
            Opcode::GetByValue,
            &[callee.index(), call_base.index(), property.index()],
            span,
        )?;
        let mut argument_slots = Vec::with_capacity(arguments.len());
        for _ in arguments {
            argument_slots.push(self.register()?);
        }
        for (argument, slot) in arguments.iter().zip(argument_slots) {
            let value = self.expression(argument)?;
            self.emit(Opcode::Move, &[slot.index(), value.index()], argument.span)?;
        }
        let destination = self.register()?;
        let argument_count =
            u32::try_from(arguments.len()).map_err(|_| CompileError::RegisterOverflow)?;
        self.emit(
            Opcode::CallWithReceiver,
            &[destination.index(), call_base.index(), argument_count],
            span,
        )?;
        Ok(destination)
    }

    /// Emits both arms into one result register and resolves their labels before bytecode becomes immutable.
    pub(in crate::bytecode) fn conditional(
        &mut self,
        test: &HirExpression,
        consequent: &HirExpression,
        alternate: &HirExpression,
        span: SourceSpan,
    ) -> Result<RegisterId, CompileError> {
        let test = self.expression(test)?;
        let alternate_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let end_label = self.builder.new_label().map_err(CompileError::Builder)?;
        let destination = self.register()?;
        let source_span = BytecodeSourceSpan {
            start: span.start,
            end: span.end,
        };
        self.builder
            .emit_jump_if_false(test, alternate_label, source_span)
            .map_err(CompileError::Builder)?;
        let consequent = self.expression(consequent)?;
        self.emit(
            Opcode::Move,
            &[destination.index(), consequent.index()],
            span,
        )?;
        self.builder
            .emit_jump(end_label, source_span)
            .map_err(CompileError::Builder)?;
        self.builder
            .bind_label(alternate_label)
            .map_err(CompileError::Builder)?;
        let alternate = self.expression(alternate)?;
        self.emit(
            Opcode::Move,
            &[destination.index(), alternate.index()],
            span,
        )?;
        self.builder
            .bind_label(end_label)
            .map_err(CompileError::Builder)?;
        Ok(destination)
    }
}
