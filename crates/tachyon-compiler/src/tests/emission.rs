use super::*;

fn string_constants(module: &tachyon_bytecode::CompiledModule) -> Vec<&[u16]> {
    module
        .constants()
        .iter()
        .filter_map(|constant| match constant {
            tachyon_bytecode::BytecodeConstant::String(value) => Some(value.as_ref()),
            _ => None,
        })
        .collect()
}

#[test]
fn compiler_preserves_bigint_literals_as_exact_decimal_constants() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "0x10n; 1_000_000_000_000_000_000_000n;",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let constants: Vec<_> = module
        .constants()
        .iter()
        .filter_map(|constant| match constant {
            tachyon_bytecode::BytecodeConstant::BigInt(value) => Some(value.as_ref()),
            _ => None,
        })
        .collect();

    assert_eq!(constants, ["16", "1000000000000000000000"]);
}

#[test]
fn compiler_preserves_string_literal_utf16_code_units() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "['\\n\\x41', '\\uD800', '\\uDFFF', '\\uD83D\\uDE00', '\\uFFFD', '\\uFFFD\\uD800'];",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let strings = string_constants(&module);

    assert!(strings.contains(&[0x000a, 0x0041].as_slice()));
    assert!(strings.contains(&[0xd800].as_slice()));
    assert!(strings.contains(&[0xdfff].as_slice()));
    assert!(strings.contains(&[0xd83d, 0xde00].as_slice()));
    assert!(strings.contains(&[0xfffd].as_slice()));
    assert!(strings.contains(&[0xfffd, 0xd800].as_slice()));
}

#[test]
fn compiler_emits_distinct_tagged_template_site_constants_and_receiver_calls() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "receiver.tag`same${first}`; receiver.tag`same${second}`;",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let sites: Vec<_> = module
        .constants()
        .iter()
        .filter_map(|constant| match constant {
            tachyon_bytecode::BytecodeConstant::TemplateSite { cooked, raw } => Some((cooked, raw)),
            _ => None,
        })
        .collect();
    assert_eq!(
        sites.len(),
        2,
        "identical source sites must not be interned"
    );
    assert_eq!(sites[0], sites[1]);
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(disassembly.matches("LoadTemplateObject").count(), 2);
    assert_eq!(disassembly.matches("CallWithReceiver").count(), 2);
    for section in disassembly.split("CallWithReceiver").take(2) {
        let getter = section
            .rfind("GetById")
            .expect("tag getter must be emitted");
        let template = section
            .rfind("LoadTemplateObject")
            .expect("template load must be emitted");
        assert!(
            getter < template,
            "tag getter must run before template lookup"
        );
    }
}

#[test]
fn compiler_never_marks_a_tagged_eval_as_direct_eval() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "eval`source`;"),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("LoadTemplateObject"));
    assert!(disassembly.contains("Call"));
    assert!(!disassembly.contains("DirectEval"));
}

#[test]
fn compiler_preserves_private_and_super_tag_receivers() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "class Base { tag(parts) { return parts; } } class Derived extends Base { #tag(parts) { return parts; } privateSite() { return this.#tag`private`; } superSite() { return super.tag`super`; } computedSuperSite() { return super['tag']`computed`; } }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let disassemblies: Vec<_> = module
        .functions()
        .iter()
        .map(|function| tachyon_bytecode::disassemble(function).unwrap())
        .collect();
    assert!(
        disassemblies
            .iter()
            .any(|text| { text.contains("GetPrivate") && text.contains("LoadTemplateObject") })
    );
    assert!(
        disassemblies
            .iter()
            .any(|text| { text.contains("GetSuperById") && text.contains("LoadTemplateObject") })
    );
    assert!(
        disassemblies.iter().any(|text| {
            text.contains("GetSuperByValue") && text.contains("LoadTemplateObject")
        })
    );
}

#[test]
fn compiler_emits_optional_chain_guards_before_deferred_operands() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "base?.[computed()]?.method(argument());",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    let first_guard = disassembly
        .find("JumpIfNotNullish")
        .expect("optional member must emit a guard");
    let computed_call = disassembly[first_guard..]
        .find("Call")
        .map(|offset| first_guard + offset)
        .expect("computed key must be called after the guard");
    let second_guard = disassembly[computed_call..]
        .find("JumpIfNotNullish")
        .map(|offset| computed_call + offset)
        .expect("optional method access must emit a second guard");
    let argument_call = disassembly[second_guard..]
        .find("Call")
        .map(|offset| second_guard + offset)
        .expect("argument must be called after the second guard");
    assert!(first_guard < computed_call && computed_call < second_guard);
    assert!(second_guard < argument_call);
}

#[test]
fn compiler_preserves_optional_call_receivers_and_indirect_eval() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "receiver.method?.(); (receiver?.method)(); eval?.('source');",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.matches("CallWithReceiver").count() >= 2);
    assert!(!disassembly.contains("DirectEval"));
}

#[test]
fn compiler_supports_super_new_target_delete_and_tail_optional_calls() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "delete target?.property; function ordinary() { return new.target?.(); } class Base { method() {} } class Derived extends Base { method() { return super.method?.(); } }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let disassemblies = module
        .functions()
        .iter()
        .map(|function| tachyon_bytecode::disassemble(function).unwrap())
        .collect::<Vec<_>>();
    assert!(disassemblies.iter().any(|text| text.contains("DeleteById")));
    assert!(
        disassemblies
            .iter()
            .any(|text| text.contains("LoadNewTarget") && text.contains("Call"))
    );
    assert!(
        disassemblies
            .iter()
            .any(|text| { text.contains("GetSuperById") && text.contains("TailCallWithReceiver") })
    );
}

#[test]
fn compiler_supports_spread_inside_an_optional_call_after_its_guard() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "callable?.(...argumentsList);"),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    let guard = disassembly.find("JumpIfNotNullish").unwrap();
    let iterator = disassembly.find("CreateArray").unwrap();
    let call = disassembly.find("CallSpread").unwrap();
    assert!(guard < iterator && iterator < call);
}

#[test]
fn compiler_preserves_lone_surrogates_in_directives_and_property_keys() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "'\\uD800'; 'use strict'; var object = { '\\uD800': 1, get '\\uDFFF'() { return 2; } }; var { '\\uD800': value } = object; class Box { '\\uD800'() {} }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let strings = string_constants(&module);

    assert_eq!(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap()
            .strictness(),
        tachyon_bytecode::FunctionStrictness::Strict
    );
    assert!(strings.contains(&[0xd800].as_slice()));
    assert!(strings.contains(&[0xdfff].as_slice()));
    assert!(
        module
            .scope_names()
            .iter()
            .all(|name| !name.contains("fffd"))
    );
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("CreateDataPropertyByValue"));
}

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
fn compiler_emits_dynamic_import_after_source_then_options() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "import(sourceValue(), optionsValue());",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    let source = disassembly.find("Call r1, callee=r0, argc=0").unwrap();
    let options = disassembly.find("Call r3, callee=r2, argc=0").unwrap();
    let dynamic_import = disassembly
        .find("DynamicImport r4, source=r1, options=r3")
        .unwrap();
    assert!(
        source < options && options < dynamic_import,
        "{disassembly}"
    );
}

#[test]
fn compiler_materializes_undefined_for_missing_dynamic_import_options() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "import('source.js');"),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("LoadUndefined r1"), "{disassembly}");
    assert!(
        disassembly.contains("DynamicImport r2, source=r0, options=r1"),
        "{disassembly}"
    );
}

#[test]
fn compiler_uses_own_definition_for_array_elements_but_not_synthetic_length() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "[11];"),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();

    assert!(disassembly.contains("CreateDataPropertyById target=r0, value=r1, name=0"));
    assert!(disassembly.contains("SetById receiver=r0,"));
    assert!(disassembly.contains("name=1"));
    assert!(!disassembly.contains("CreateDataPropertyByValue"));
    assert_eq!(module.scope_names(), &[Arc::from("0"), Arc::from("length")]);
}

#[test]
fn compiler_uses_own_definition_for_object_literal_data_properties() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "({ fixed: 1, ['computed']: 2 });"),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();

    assert!(disassembly.contains("CreateDataPropertyById"));
    assert!(disassembly.contains("CreateDataPropertyByValue"));
    assert!(!disassembly.contains("SetById"));
    assert!(!disassembly.contains("SetByValue"));
}

#[test]
fn compiler_materializes_the_arguments_object_in_function_scope() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "function read() { return arguments; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(1))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("LoadArgumentsObject"));
    assert!(!disassembly.contains("LoadArgumentsLength"));
}

#[test]
fn compiler_captures_outer_arguments_only_for_arrows_that_reference_it() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "function outer() { return () => arguments; } function plain() { return () => 1; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let outer = module
        .function(tachyon_bytecode::FunctionId::new(2))
        .unwrap();
    let arrow = module
        .function(tachyon_bytecode::FunctionId::new(1))
        .unwrap();
    let plain = module
        .function(tachyon_bytecode::FunctionId::new(4))
        .unwrap();
    assert_eq!(outer.layout().environment_slot_count, 1);
    assert!(outer.layout().needs_argument_source);
    assert!(
        tachyon_bytecode::disassemble(outer)
            .unwrap()
            .contains("LoadArgumentsObject")
    );
    assert!(
        tachyon_bytecode::disassemble(arrow)
            .unwrap()
            .contains("LoadEnvironment")
    );
    assert_eq!(plain.layout().environment_slot_count, 0);
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
    assert!(disassembly.contains("Update r0, r0, increment"));
    assert!(!disassembly.contains("Move"));
}

#[test]
/// Confirms script vars/functions declare globals while function-local vars use frames.
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
    assert_eq!(entry.matches("DeclareScope").count(), 3);
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
