use super::*;

#[test]
fn compiler_lowers_array_binding_with_elision_and_default() {
    Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "const [first, , third = 3] = source; first + third;",
            ),
            CompileOptions::default(),
        )
        .unwrap();
}

#[test]
fn compiler_lowers_array_assignment_pattern() {
    Compiler
        .compile(
            source(MediaType::JavaScript, "let first; [first] = source; first;"),
            CompileOptions::default(),
        )
        .unwrap();
}

#[test]
fn compiler_emits_verified_bytecode_for_one_plus_two() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "1 + 2;"),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("LoadImmediate r0, imm=1"));
    assert!(disassembly.contains("LoadImmediate r1, imm=2"));
    assert!(disassembly.contains("Add r2, r0, r1"));
    assert!(disassembly.contains("Return r2"));
}

#[test]
fn compiler_keeps_loose_and_strict_inequality_opcodes_distinct() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "'0' != 0; '0' !== 0;"),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(disassembly.matches("LooseEqual").count(), 1);
    assert_eq!(disassembly.matches("StrictEqual").count(), 1);
    assert_eq!(disassembly.matches("Not").count(), 2);
}

#[test]
fn function_hir_owns_parameters_body_and_direct_call() {
    let hir = Compiler
        .lower_to_hir(
            source(
                MediaType::JavaScript,
                "function addTwo(value) { return value + 2; } addTwo(40);",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let [function] = hir.functions() else {
        panic!("expected one owned function stencil");
    };
    assert_eq!(function.name.as_deref(), Some("addTwo"));
    let HirPattern {
        kind: HirPatternKind::Binding(binding),
        ..
    } = &function.parameters[0]
    else {
        panic!("expected simple binding parameter");
    };
    assert_eq!(binding.name.as_ref(), "value");
    assert!(matches!(function.body[0].kind, HirStatementKind::Return(_)));
    assert!(matches!(
        hir.statements().last().map(|statement| &statement.kind),
        Some(HirStatementKind::Expression(HirExpression {
            kind: HirExpressionKind::Call { .. },
            ..
        }))
    ));
}

#[test]
/// Preserves parameter initializer expressions in owned HIR after the Oxc arena is discarded.
fn function_hir_owns_default_parameter_initializers() {
    let hir = Compiler
        .lower_to_hir(
            source(
                MediaType::JavaScript,
                "function add(value = 40, next = value + 1) { return next; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let [function] = hir.functions() else {
        panic!("expected one function stencil");
    };
    assert_eq!(function.parameter_initializers.len(), 2);
    assert!(function.parameter_initializers[0].is_some());
    assert!(matches!(
        function.parameter_initializers[1],
        Some(HirExpression {
            kind: HirExpressionKind::Binary { .. },
            ..
        })
    ));
}

#[test]
fn compiler_emits_hoisted_closure_call_and_ordinary_function() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "addTwo(40); function addTwo(value) { return value + 2; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    assert_eq!(module.functions().len(), 2);
    let entry = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    let function = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(1))
            .unwrap(),
    )
    .unwrap();
    assert!(entry.contains("CreateClosure r0, function=1"));
    assert!(entry.contains("StoreScope r0, scope=0"));
    assert_eq!(module.scope_names(), &[Arc::from("addTwo")]);
    assert!(entry.contains("Call "));
    assert!(entry.contains("argc=1"));
    assert!(function.contains("Add r2, r0, r1"));
    assert!(function.contains("Return r2"));
}

#[test]
/// Empty and bare-return functions require no register solely to materialize undefined.
fn compiler_uses_zero_register_return_undefined_for_known_completions() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "function implicit() {} function explicit() { return; } implicit(); explicit();",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    for function_id in [1, 2] {
        let function = module
            .function(tachyon_bytecode::FunctionId::new(function_id))
            .unwrap();
        let disassembly = tachyon_bytecode::disassemble(function).unwrap();
        assert_eq!(function.layout().register_count, 0);
        assert!(disassembly.contains("ReturnUndefined"));
        assert!(!disassembly.contains("LoadUndefined"));
    }
}

#[test]
fn compiler_compacts_zero_argument_call_loop_hot_path() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "function f() {} function main() { for (let i = 0; i < 100_000; i++) { f(); } }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let main = module
        .functions()
        .iter()
        .find(|function| function.layout().name_scope == Some(1))
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(main).unwrap();
    assert!(disassembly.contains("Call r4, callee=r3, argc=0"));
    assert!(disassembly.contains("Add r0, r0,"));
    assert!(!disassembly.contains("Move"));
}

#[test]
/// Confirms script vars become deduplicated global declarations while function vars use frames.
fn compiler_instantiates_var_bindings_at_their_scope_entry() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "var outer; { var inner = 2; } function read(value) { return typeof local; var local = value; } inner;",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let entry = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    let function = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(1))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(entry.matches("DeclareScope").count(), 2);
    assert!(entry.contains("StoreScope"));
    assert!(function.contains("LoadUndefined"));
    assert!(function.contains("Typeof"));
}

#[test]
/// A fully terminal try/catch must not leave a jump whose target is the function code end.
fn terminal_try_catch_freezes_only_instruction_boundary_targets() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "function render(value) { var basic = value; if (basic) return basic; try { return value; } catch (error) { if (error) { return 1; } throw error; } } render(2);",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let function = module
        .function(tachyon_bytecode::FunctionId::new(1))
        .unwrap();
    assert!(tachyon_bytecode::disassemble(function).is_ok());
}

#[test]
fn compiler_lowers_instanceof_to_its_verified_opcode() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "function Constructor() {} new Constructor() instanceof Constructor;",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let entry = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(entry.contains("InstanceOf"));
}

#[test]
fn compiler_lowers_nonlocal_identifier_writes_to_resolved_scope_stores() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "var value = 1; function update() { value += 2; value++; } update(); value = 7;",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let entry = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    let function = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(1))
            .unwrap(),
    )
    .unwrap();
    assert!(entry.contains("StoreResolvedScope"));
    assert_eq!(function.matches("StoreResolvedScope").count(), 2);
}

#[test]
/// Proves compiler-selected frame/global locations are frozen alongside verified functions.
fn compiler_emits_binding_plans_for_local_and_global_storage() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "var global = 1; function read(param) { let local = param; const fixed = local; return fixed + global; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let entry = module
        .function(tachyon_bytecode::FunctionId::new(0))
        .unwrap();
    let function = module
        .function(tachyon_bytecode::FunctionId::new(1))
        .unwrap();

    assert!(entry.binding_plan().iter().any(|binding| {
        binding.location == tachyon_bytecode::BindingLocation::GlobalProperty
            && binding.name.as_ref() == "global"
    }));
    assert_eq!(
        function
            .binding_plan()
            .iter()
            .filter(|binding| matches!(
                binding.location,
                tachyon_bytecode::BindingLocation::FrameRegister(_)
            ))
            .count(),
        3
    );
    assert!(
        function
            .binding_plan()
            .iter()
            .any(|binding| { !binding.mutable && binding.name.as_ref() == "fixed" })
    );
    assert!(function.binding_plan().iter().any(|binding| {
        binding.location == tachyon_bytecode::BindingLocation::GlobalProperty
            && binding.name.as_ref() == "global"
    }));
}
